//! Native, non-limiting, fail-open calibrated decision support.
//!
//! Designed for calibrated decision-scoring models such as
//! `microsoft/microsoft-decision-1.1` on OpenRouter, and also compatible with
//! any fast chat model configured in `[routing] decision_model`.
//!
//! Core invariants:
//! - When `settings.routing.decision_model` is `None` or empty, ZERO network
//!   calls occur.
//! - Every call uses a tight timeout (<= 2.5s), zero retries, and respects
//!   [`Cancel`]; any error, timeout, or unparseable output silently falls back
//!   to normal behavior.
//! - Decisions NEVER downgrade complex tasks, NEVER remove tool schemas that
//!   deterministic keywords/sticky groups already unlocked, NEVER hard-block a
//!   command, and NEVER switch the main conversation model mid-session.

use std::collections::HashMap;

use serde_json::Value;
use zerith_core::Settings;
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig, RetryPolicy};

/// Upper bound on every auxiliary decision call so a slow or dead endpoint
/// can never stall a turn.
pub const DECISION_TIMEOUT_MS: u64 = 2_500;

const EFFORT_OPTIONS: &[&str] = &["low", "medium", "high"];
const RISK_OPTIONS: &[&str] = &["routine_safe", "caution_change", "lockout_or_destructive"];
const TOOL_GROUP_OPTIONS: &[&str] = &["general", "rede", "workflows", "browser"];
const GOAL_OPTIONS: &[&str] = &["done", "continue", "blocked"];

const EFFORT_SYSTEM: &str = "Score the reasoning effort needed to answer the user's request well without losing quality. \
Options: low (trivial greeting/fact/short edit), medium (normal coding/ops task), high (debugging, architecture, network/BGP/OSPF/OLT config, security, multi-step analysis). \
Reply ONLY with a JSON object mapping each option (\"low\", \"medium\", \"high\") to a calibrated probability in [0,1] summing to 1.";

const RISK_SYSTEM: &str = "Classify the operational risk of executing this command on a host or network device. \
Options: routine_safe (benign state check or low-impact routine), caution_change (state/config change needing review), lockout_or_destructive (management lockout, reboot/reload, wipe/format, mass delete, firewall/routing cut). \
Reply ONLY with a JSON object mapping each option (\"routine_safe\", \"caution_change\", \"lockout_or_destructive\") to a calibrated probability in [0,1] summing to 1.";

const TOOL_GROUP_SYSTEM: &str = "Classify whether the user's request clearly needs a specialized tool group not already obvious from keywords. \
Options: general (default coding/files/shell/chat), rede (network devices, routers, switches, OLTs, SSH/Telnet/SNMP/NOC), workflows (scheduled automations, recurring jobs, channels), browser (web UI interaction, clicking, scraping rendered pages). \
Reply ONLY with a JSON object mapping each option (\"general\", \"rede\", \"workflows\", \"browser\") to a calibrated probability in [0,1] summing to 1.";

const GOAL_SYSTEM: &str = "Score whether the /goal objective is completely verified as done based strictly on the evidence (commands, exit codes, edits, verification). \
Options: done (evidence clearly proves full completion), continue (more work or verification needed), blocked (requires user input/credentials or impossible). \
Reply ONLY with a JSON object mapping each option (\"done\", \"continue\", \"blocked\") to a calibrated probability in [0,1] summing to 1.";

/// Informative risk banner shown alongside an approval prompt. Purely advisory:
/// never blocks execution or changes the policy verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskAdvisory {
    pub label: &'static str,
    pub confidence_pct: u8,
    pub note: String,
}

impl RiskAdvisory {
    pub fn banner(&self) -> String {
        format!(
            "[Decisão · {}% {}] {}",
            self.confidence_pct, self.label, self.note
        )
    }
}

/// Fail-open auxiliary client bound to `settings.routing.decision_model`.
pub struct DecisionClient {
    client: Client,
    model: String,
}

impl DecisionClient {
    /// Builds a decision client when `settings.routing.decision_model` is set
    /// and non-empty. Performs zero network I/O.
    pub fn from_settings(
        settings: Option<&Settings>,
        fallback_cfg: &ProviderConfig,
    ) -> Option<Self> {
        let settings = settings?;
        let spec = settings
            .routing
            .decision_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let (gw, _) = crate::resolve_gateway(settings, spec, fallback_cfg);
        let mut cand = gw.candidates.into_iter().next()?;
        if cand.model.trim().is_empty() {
            return None;
        }
        cand.config.timeout_ms = if cand.config.timeout_ms == 0 {
            DECISION_TIMEOUT_MS
        } else {
            cand.config.timeout_ms.min(DECISION_TIMEOUT_MS)
        };
        let client = Client::new(cand.config)
            .ok()?
            .with_retry(RetryPolicy::none());
        Some(Self {
            client,
            model: cand.model,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_client(client: Client, model: impl Into<String>) -> Self {
        Self {
            client: client.with_retry(RetryPolicy::none()),
            model: model.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn score_options(
        &self,
        system: &str,
        user: &str,
        options: &[&str],
        cancel: &Cancel,
    ) -> Option<HashMap<String, f64>> {
        if cancel.is_cancelled() {
            return None;
        }
        let req = ChatRequest::new(
            &self.model,
            vec![
                ChatMessage::system(system),
                ChatMessage::synthetic_user(user),
            ],
        )
        .max_tokens(128)
        .temperature(0.0);
        let comp = self.client.complete(&req, cancel).ok()?;
        parse_scores(&comp.text, options)
    }

    /// Chooses an effective reasoning effort (`"low"`, `"medium"`, or `"high"`)
    /// when the user picked `"auto"`. Always falls back to [`heuristic_effort`]
    /// on timeout, error, cancel, or unparseable output.
    pub fn decide_effort(
        &self,
        prompt: &str,
        recent_summary: &str,
        cancel: &Cancel,
    ) -> &'static str {
        let prompt_clip = clip_chars(prompt, 1_200);
        let recent_clip = clip_chars(recent_summary, 600);
        let user = if recent_clip.is_empty() {
            format!("Request:\n{prompt_clip}")
        } else {
            format!("Recent context:\n{recent_clip}\n\nRequest:\n{prompt_clip}")
        };
        match self.score_options(EFFORT_SYSTEM, &user, EFFORT_OPTIONS, cancel) {
            Some(scores) => effort_from_scores(&scores),
            None => heuristic_effort(prompt),
        }
    }

    /// Produces an optional informative risk advisory for a mutating/dangerous
    /// approval prompt. Purely consultative: never blocks or changes policy.
    pub fn assess_command_risk(
        &self,
        tool: &str,
        command: &str,
        vendor: Option<&str>,
        cancel: &Cancel,
    ) -> Option<RiskAdvisory> {
        let cmd_clip = clip_chars(command, 800);
        if cmd_clip.is_empty() {
            return None;
        }
        let vendor_line = vendor
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| format!("\nVendor: {v}"))
            .unwrap_or_default();
        let user = format!("Tool: {tool}{vendor_line}\nCommand:\n{cmd_clip}");
        let scores = self.score_options(RISK_SYSTEM, &user, RISK_OPTIONS, cancel)?;
        risk_from_scores(&scores)
    }

    /// Strictly additive suggestion for unlocking one extra tool group
    /// (`"rede"`, `"workflows"`, or `"browser"`) when deterministic keywords did
    /// not match. Returns `None` unless probability >= 0.75.
    pub fn suggest_extra_tool_group(&self, prompt: &str, cancel: &Cancel) -> Option<&'static str> {
        let prompt_clip = clip_chars(prompt, 1_000);
        if prompt_clip.is_empty() {
            return None;
        }
        let user = format!("Request:\n{prompt_clip}");
        let scores = self.score_options(TOOL_GROUP_SYSTEM, &user, TOOL_GROUP_OPTIONS, cancel)?;
        tool_group_from_scores(&scores)
    }

    /// Scores calibrated completion probability for `/goal`. Returns
    /// `Some(p("done"))` when calibrated scoring succeeds and
    /// `p("blocked") < 0.05`; callers only fast-exit when `>= 0.92` and
    /// evidence had no failed commands.
    pub fn score_goal_done(
        &self,
        goal: &str,
        answer_excerpt: &str,
        evidence_summary: &str,
        cancel: &Cancel,
    ) -> Option<f64> {
        let goal_clip = clip_chars(goal, 800);
        let answer_clip = clip_chars(answer_excerpt, 1_200);
        let evidence_clip = clip_chars(evidence_summary, 1_200);
        let user = format!(
            "Goal:\n{goal_clip}\n\nAssistant answer:\n{answer_clip}\n\nEvidence:\n{evidence_clip}"
        );
        let scores = self.score_options(GOAL_SYSTEM, &user, GOAL_OPTIONS, cancel)?;
        goal_done_score(&scores)
    }
}

/// Parses option probabilities from calibrated JSON, choice JSON, or a short
/// plain-text single-option reply.
pub fn parse_scores(raw: &str, options: &[&str]) -> Option<HashMap<String, f64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || options.is_empty() {
        return None;
    }

    if let Some(scores) = parse_json_scores(trimmed, options) {
        return Some(scores);
    }

    parse_plain_text_score(trimmed, options)
}

fn canonical_option(key: &str, options: &[&str]) -> Option<String> {
    let k = key.trim();
    options
        .iter()
        .find(|&&opt| opt.eq_ignore_ascii_case(k))
        .map(|&opt| opt.to_string())
}

fn normalize_prob(v: f64) -> Option<f64> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    if v <= 1.0 {
        Some(v)
    } else if v <= 100.0 {
        Some((v / 100.0).clamp(0.0, 1.0))
    } else {
        None
    }
}

fn extract_prob_map(
    obj: &serde_json::Map<String, Value>,
    options: &[&str],
) -> Option<HashMap<String, f64>> {
    let mut raw_hits: Vec<(String, f64)> = Vec::new();
    for (k, v) in obj {
        if let Some(opt) = canonical_option(k, options) {
            if let Some(num) = v.as_f64().filter(|n| n.is_finite() && *n >= 0.0) {
                raw_hits.push((opt, num));
            }
        }
    }
    if raw_hits.is_empty() {
        return None;
    }
    let scale_pct = raw_hits.iter().any(|(_, n)| *n > 1.0);
    let mut out = HashMap::new();
    for (opt, num) in raw_hits {
        let p = if scale_pct {
            if num > 100.0 {
                return None;
            }
            (num / 100.0).clamp(0.0, 1.0)
        } else {
            num.clamp(0.0, 1.0)
        };
        out.insert(opt, p);
    }
    (!out.is_empty()).then_some(out)
}

fn parse_json_scores(trimmed: &str, options: &[&str]) -> Option<HashMap<String, f64>> {
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    let val: Value = serde_json::from_str(&trimmed[start..=end]).ok()?;
    let obj = val.as_object()?;

    // Format 1: direct probability map {"low": 0.88, "medium": 0.09, "high": 0.03}
    if let Some(map) = extract_prob_map(obj, options) {
        return Some(map);
    }
    // Format 1b: nested probability map {"probabilities": {...}} / {"scores": {...}}
    for nested_key in ["probabilities", "scores", "distribution"] {
        if let Some(nested) = obj.get(nested_key).and_then(Value::as_object) {
            if let Some(map) = extract_prob_map(nested, options) {
                return Some(map);
            }
        }
    }

    // Format 2: explicit choice JSON {"choice": "high", "confidence": 0.9}
    const CHOICE_KEYS: &[&str] = &[
        "choice", "verdict", "decision", "label", "option", "group", "effort", "risk", "answer",
    ];
    const CONF_KEYS: &[&str] = &["confidence", "score", "probability", "prob"];
    for key in CHOICE_KEYS {
        if let Some(chosen) = obj
            .get(*key)
            .and_then(Value::as_str)
            .and_then(|s| canonical_option(s, options))
        {
            let conf = CONF_KEYS
                .iter()
                .find_map(|ck| {
                    obj.get(*ck)
                        .and_then(Value::as_f64)
                        .and_then(normalize_prob)
                })
                .unwrap_or(0.85);
            let mut out = HashMap::new();
            out.insert(chosen, conf);
            return Some(out);
        }
    }

    None
}

fn parse_plain_text_score(trimmed: &str, options: &[&str]) -> Option<HashMap<String, f64>> {
    // Reject JSON-like strings (already handled/rejected by `parse_json_scores`)
    // and long prose/error explanations to prevent false-positive matches.
    if trimmed.contains('{') || trimmed.contains('}') || trimmed.chars().count() > 80 {
        return None;
    }
    let cleaned = trimmed
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '*' | '.' | '!' | ',' | ':' | ';')
        })
        .trim();
    if let Some(exact) = canonical_option(cleaned, options) {
        let mut out = HashMap::new();
        out.insert(exact, 0.85);
        return Some(out);
    }

    let lower = cleaned.to_ascii_lowercase();
    let mut matched: Option<String> = None;
    for &opt in options {
        let needle = opt.to_ascii_lowercase();
        if contains_word(&lower, &needle) {
            if matched.is_some() {
                // Ambiguous: more than one option mentioned.
                return None;
            }
            matched = Some(opt.to_string());
        }
    }
    matched.map(|opt| {
        let mut out = HashMap::new();
        out.insert(opt, 0.85);
        out
    })
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut offset = 0;
    while let Some(pos) = haystack[offset..].find(needle) {
        let start = offset + pos;
        let end = start + needle.len();
        let left_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let right_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if left_ok && right_ok {
            return true;
        }
        offset = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn contains_word_prefix(haystack: &str, prefix: &str) -> bool {
    let mut offset = 0;
    while let Some(pos) = haystack[offset..].find(prefix) {
        let start = offset + pos;
        let left_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        if left_ok {
            return true;
        }
        offset = start + prefix.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Conservative quality-first thresholds for `"auto"` reasoning effort:
/// - `"low"` ONLY when `p("low") >= 0.78 && p("high") < 0.15`
/// - `"high"` when `p("high") >= 0.35`
/// - `"medium"` otherwise.
pub fn effort_from_scores(scores: &HashMap<String, f64>) -> &'static str {
    let p_low = scores.get("low").copied().unwrap_or(0.0);
    let p_high = scores.get("high").copied().unwrap_or(0.0);
    if p_low >= 0.78 && p_high < 0.15 {
        "low"
    } else if p_high >= 0.35 {
        "high"
    } else {
        "medium"
    }
}

/// Deterministic fallback for `"auto"` reasoning effort when no
/// `decision_model` is configured or a decision call fails.
///
/// Conservative and quality-first: returns `"low"` only for clearly short,
/// simple queries (`<= 80` chars without code/debug/refactor/network/config/
/// architecture markers), `"high"` for complex/long/multi-step prompts, and
/// `"medium"` otherwise.
pub fn heuristic_effort(prompt: &str) -> &'static str {
    let trimmed = prompt.trim();
    let chars = trimmed.chars().count();
    let lines = trimmed.lines().count();
    let lower = trimmed.to_lowercase();

    const HIGH_PREFIXES: &[&str] = &[
        "arquitet",
        "architect",
        "refator",
        "refactor",
        "migra",
        "root cause",
        "causa raiz",
        "auditoria",
        "security audit",
        "diagnóstico completo",
        "diagnostico completo",
        "passo a passo",
        "step by step",
        "plano completo",
        "full plan",
        "debug",
        "depur",
        "deadlock",
        "race condition",
        "memory leak",
        "otimiz",
        "optimiz",
        "tradeoff",
        "trade-off",
    ];
    const HIGH_WORDS: &[&str] = &["bgp", "ospf", "mpls", "cgnat"];
    const SIMPLE_BLOCKER_PREFIXES: &[&str] = &[
        "código",
        "codigo",
        "config",
        "mikrotik",
        "routeros",
        "huawei",
        "cisco",
        "juniper",
        "fortigate",
        "firewall",
        "roteador",
        "router",
        "switch",
        "crash",
        "panic",
        "stack",
        "trace",
        "regex",
        "typescript",
        "python",
        "cargo",
        "docker",
        "kubernetes",
        "segurança",
        "security",
        "audit",
        "implement",
        "corrig",
        "analis",
        "analyz",
        "diagn",
        "script",
        "deploy",
        "produção",
        "producao",
    ];
    const SIMPLE_BLOCKER_WORDS: &[&str] = &[
        "code", "olt", "onu", "gpon", "vlan", "pppoe", "ssh", "erro", "erros", "error", "errors",
        "bug", "bugs", "sql", "rust", "k8s", "fix", "test", "tests", "teste", "testes", "api",
        "apis",
    ];

    let has_high = HIGH_PREFIXES
        .iter()
        .any(|m| contains_word_prefix(&lower, m))
        || HIGH_WORDS.iter().any(|w| contains_word(&lower, w));
    let multi_step = chars >= 350
        || lines >= 5
        || (chars >= 140 && (trimmed.contains("\n1.") || trimmed.contains("\n- ")));
    if has_high || multi_step {
        return "high";
    }

    let has_blocker = trimmed.contains("```")
        || SIMPLE_BLOCKER_PREFIXES
            .iter()
            .any(|m| contains_word_prefix(&lower, m))
        || SIMPLE_BLOCKER_WORDS
            .iter()
            .any(|w| contains_word(&lower, w));
    if chars <= 80 && lines <= 2 && !has_blocker {
        "low"
    } else {
        "medium"
    }
}

/// Converts calibrated risk probabilities into an optional informative
/// advisory. Returns `None` when probabilities are ambiguous.
pub fn risk_from_scores(scores: &HashMap<String, f64>) -> Option<RiskAdvisory> {
    let p_high = scores.get("lockout_or_destructive").copied().unwrap_or(0.0);
    let p_caution = scores.get("caution_change").copied().unwrap_or(0.0);
    let p_safe = scores.get("routine_safe").copied().unwrap_or(0.0);
    let pct = |p: f64| (p.clamp(0.0, 1.0) * 100.0).round() as u8;

    if p_high >= 0.45 {
        Some(RiskAdvisory {
            label: "risco alto",
            confidence_pct: pct(p_high),
            note: "Possível impacto de gerência/indisponibilidade ou alteração destrutiva — confirme antes de aprovar.".into(),
        })
    } else if p_caution >= 0.50 {
        Some(RiskAdvisory {
            label: "cautela",
            confidence_pct: pct(p_caution),
            note: "Alteração de estado/configuração — revise os parâmetros.".into(),
        })
    } else if p_safe >= 0.92 {
        Some(RiskAdvisory {
            label: "baixo risco",
            confidence_pct: pct(p_safe),
            note: "Operação rotineira sem indício de impacto crítico.".into(),
        })
    } else {
        None
    }
}

/// Strictly additive tool group selection: returns `Some(group)` only when
/// `group != "general"` and `p(group) >= 0.75`.
pub fn tool_group_from_scores(scores: &HashMap<String, f64>) -> Option<&'static str> {
    const SPECIALIZED: [&str; 3] = ["rede", "workflows", "browser"];
    SPECIALIZED
        .iter()
        .filter_map(|&g| {
            let p = scores.get(g).copied().unwrap_or(0.0);
            (p >= 0.75).then_some((g, p))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(g, _)| g)
}

/// Returns `Some(p("done"))` when calibrated scores include `"done"` and
/// `p("blocked") < 0.05`.
pub fn goal_done_score(scores: &HashMap<String, f64>) -> Option<f64> {
    let p_done = scores.get("done").copied()?;
    let p_blocked = scores.get("blocked").copied().unwrap_or(0.0);
    if p_blocked >= 0.05 {
        return None;
    }
    Some(p_done)
}

/// Predicate for `/goal` fast-exit as `done`: requires `p("done") >= 0.92`,
/// `p("blocked") < 0.05`, and zero failed commands in evidence.
pub fn should_fast_exit_goal_done(
    scores: &HashMap<String, f64>,
    has_failed_commands: bool,
) -> bool {
    !has_failed_commands && goal_done_score(scores).is_some_and(|p| p >= 0.92)
}

fn clip_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use zerith_providers::ProviderKind;

    fn map(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    #[test]
    fn from_settings_is_none_without_decision_model_and_clamps_timeout() {
        let mut fallback = ProviderConfig::new(ProviderKind::OpenAi);
        fallback.base_url = "http://127.0.0.1:9999/v1".into();
        fallback.api_key = Some("sk-test".into());
        fallback.timeout_ms = 60_000;

        assert!(DecisionClient::from_settings(None, &fallback).is_none());

        let mut settings = Settings::default();
        assert!(DecisionClient::from_settings(Some(&settings), &fallback).is_none());

        settings.routing.decision_model = Some("   ".into());
        assert!(DecisionClient::from_settings(Some(&settings), &fallback).is_none());

        settings.routing.decision_model = Some("microsoft/microsoft-decision-1.1".into());
        let dc = DecisionClient::from_settings(Some(&settings), &fallback).unwrap();
        assert_eq!(dc.model(), "microsoft/microsoft-decision-1.1");
        assert!(dc.client.config().timeout_ms <= DECISION_TIMEOUT_MS);
        assert_eq!(dc.client.retry_policy().max_retries, 0);
    }

    #[test]
    fn parse_scores_handles_calibrated_json_fences_choice_plaintext_and_garbage() {
        // 1. Calibrated probability map JSON (Microsoft Decision 1.1 format)
        let s = parse_scores(
            r#"{"low": 0.88, "medium": 0.09, "high": 0.03}"#,
            EFFORT_OPTIONS,
        )
        .unwrap();
        assert!((s["low"] - 0.88).abs() < 1e-6);
        assert!((s["medium"] - 0.09).abs() < 1e-6);
        assert!((s["high"] - 0.03).abs() < 1e-6);

        // 1b. Markdown code fence + surrounding text
        let fenced =
            "Calibrated scores:\n```json\n{\"LOW\": 0.12, \"Medium\": 0.28, \"high\": 0.60}\n```\n";
        let s = parse_scores(fenced, EFFORT_OPTIONS).unwrap();
        assert!((s["low"] - 0.12).abs() < 1e-6);
        assert!((s["medium"] - 0.28).abs() < 1e-6);
        assert!((s["high"] - 0.60).abs() < 1e-6);

        // 1c. Nested probabilities object
        let nested = r#"{"probabilities": {"done": 0.94, "continue": 0.05, "blocked": 0.01}}"#;
        let s = parse_scores(nested, GOAL_OPTIONS).unwrap();
        assert!((s["done"] - 0.94).abs() < 1e-6);

        // 2. Explicit choice JSON
        let s = parse_scores(r#"{"choice": "high", "confidence": 0.9}"#, EFFORT_OPTIONS).unwrap();
        assert!((s["high"] - 0.9).abs() < 1e-6);

        let s = parse_scores(r#"{"verdict": "done", "score": 0.95}"#, GOAL_OPTIONS).unwrap();
        assert!((s["done"] - 0.95).abs() < 1e-6);

        let s = parse_scores(r#"{"choice": "rede"}"#, TOOL_GROUP_OPTIONS).unwrap();
        assert!((s["rede"] - 0.85).abs() < 1e-6);

        // 3. Plain-text single-option fallback
        let s = parse_scores("high", EFFORT_OPTIONS).unwrap();
        assert!((s["high"] - 0.85).abs() < 1e-6);

        let s = parse_scores("**workflows**.", TOOL_GROUP_OPTIONS).unwrap();
        assert!((s["workflows"] - 0.85).abs() < 1e-6);

        // Ambiguous / garbage / oversized inputs return None
        assert!(parse_scores("low or high", EFFORT_OPTIONS).is_none());
        assert!(parse_scores("not an option at all", EFFORT_OPTIONS).is_none());
        assert!(parse_scores("", EFFORT_OPTIONS).is_none());
        assert!(parse_scores("{broken json", EFFORT_OPTIONS).is_none());
        assert!(parse_scores(r#"{"low": -0.5}"#, EFFORT_OPTIONS).is_none());
        let long_prose = "This is a very long error explanation that exceeds eighty characters and casually mentions low in passing.";
        assert!(parse_scores(long_prose, EFFORT_OPTIONS).is_none());
    }

    #[test]
    fn effort_thresholds_are_conservative_and_quality_first() {
        // Low only when p(low) >= 0.78 AND p(high) < 0.15
        assert_eq!(
            effort_from_scores(&map(&[("low", 0.80), ("medium", 0.15), ("high", 0.05)])),
            "low"
        );
        // Low = 0.77 is below threshold -> medium (never downgrades hastily)
        assert_eq!(
            effort_from_scores(&map(&[("low", 0.77), ("medium", 0.20), ("high", 0.03)])),
            "medium"
        );
        // Even if low >= 0.78, high >= 0.15 prevents downgrading to low
        assert_eq!(
            effort_from_scores(&map(&[("low", 0.80), ("medium", 0.05), ("high", 0.15)])),
            "medium"
        );
        // High >= 0.35 escalates to high
        assert_eq!(
            effort_from_scores(&map(&[("low", 0.20), ("medium", 0.45), ("high", 0.35)])),
            "high"
        );
        // Moderate distribution -> medium
        assert_eq!(
            effort_from_scores(&map(&[("low", 0.30), ("medium", 0.50), ("high", 0.20)])),
            "medium"
        );
    }

    #[test]
    fn heuristic_effort_protects_technical_and_complex_prompts() {
        assert_eq!(heuristic_effort("oi, tudo bem?"), "low");
        assert_eq!(heuristic_effort("qual a capital da França?"), "low");
        // Short but technical -> never low
        assert_eq!(heuristic_effort("verifique o config do roteador"), "medium");
        assert_eq!(heuristic_effort("ajuste a vlan na olt"), "medium");
        assert_eq!(heuristic_effort("corrija o erro no código"), "medium");
        // Complex / high markers -> high
        assert_eq!(heuristic_effort("depure o flapping BGP no core"), "high");
        assert_eq!(
            heuristic_effort("faça a arquitetura da migração OSPF"),
            "high"
        );
        // Long prompt -> high
        let long = "preciso organizar os documentos da empresa ".repeat(12);
        assert_eq!(heuristic_effort(&long), "high");
    }

    #[test]
    fn risk_advisory_formats_high_caution_low_and_omits_ambiguous() {
        let high = risk_from_scores(&map(&[
            ("lockout_or_destructive", 0.62),
            ("caution_change", 0.30),
            ("routine_safe", 0.08),
        ]))
        .unwrap();
        assert_eq!(high.label, "risco alto");
        assert_eq!(high.confidence_pct, 62);
        assert!(high.banner().starts_with("[Decisão · 62% risco alto] "));

        let caution = risk_from_scores(&map(&[
            ("lockout_or_destructive", 0.20),
            ("caution_change", 0.55),
            ("routine_safe", 0.25),
        ]))
        .unwrap();
        assert_eq!(caution.label, "cautela");
        assert_eq!(caution.confidence_pct, 55);
        assert!(caution.banner().starts_with("[Decisão · 55% cautela] "));

        let low = risk_from_scores(&map(&[
            ("lockout_or_destructive", 0.01),
            ("caution_change", 0.05),
            ("routine_safe", 0.94),
        ]))
        .unwrap();
        assert_eq!(low.label, "baixo risco");
        assert_eq!(low.confidence_pct, 94);
        assert!(low.banner().starts_with("[Decisão · 94% baixo risco] "));

        // Ambiguous -> None
        assert!(risk_from_scores(&map(&[
            ("lockout_or_destructive", 0.30),
            ("caution_change", 0.40),
            ("routine_safe", 0.30),
        ]))
        .is_none());
        assert!(risk_from_scores(&map(&[
            ("lockout_or_destructive", 0.02),
            ("caution_change", 0.10),
            ("routine_safe", 0.88),
        ]))
        .is_none());
    }

    #[test]
    fn tool_group_threshold_is_strictly_additive_and_ignores_general() {
        assert_eq!(
            tool_group_from_scores(&map(&[("general", 0.95), ("rede", 0.05)])),
            None
        );
        assert_eq!(
            tool_group_from_scores(&map(&[("general", 0.26), ("rede", 0.74)])),
            None
        );
        assert_eq!(
            tool_group_from_scores(&map(&[("general", 0.20), ("rede", 0.75)])),
            Some("rede")
        );
        assert_eq!(
            tool_group_from_scores(&map(&[("general", 0.10), ("workflows", 0.82)])),
            Some("workflows")
        );
        assert_eq!(
            tool_group_from_scores(&map(&[("general", 0.05), ("browser", 0.91)])),
            Some("browser")
        );
    }

    #[test]
    fn goal_fast_done_requires_high_confidence_low_blocked_and_no_failures() {
        let clean_done = map(&[("done", 0.94), ("continue", 0.05), ("blocked", 0.01)]);
        assert!(should_fast_exit_goal_done(&clean_done, false));
        // Any failed command disables fast-exit so full judge inspects it
        assert!(!should_fast_exit_goal_done(&clean_done, true));

        // Below 0.92 -> fall through to full judge
        let borderline = map(&[("done", 0.91), ("continue", 0.08), ("blocked", 0.01)]);
        assert!(!should_fast_exit_goal_done(&borderline, false));

        // Blocked >= 0.05 -> fall through even if done >= 0.92
        let risky_blocked = map(&[("done", 0.93), ("continue", 0.02), ("blocked", 0.05)]);
        assert_eq!(goal_done_score(&risky_blocked), None);
        assert!(!should_fast_exit_goal_done(&risky_blocked, false));
    }

    fn spawn_mock_server(status: u16, content: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_bg = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                hits_bg.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream);
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
                if content_len > 0 {
                    let _ = reader.read_exact(&mut vec![0u8; content_len]);
                }
                let body = if status == 200 {
                    serde_json::json!({
                        "choices": [{"message": {"role": "assistant", "content": content}}]
                    })
                    .to_string()
                } else {
                    r#"{"error":"upstream failed"}"#.to_string()
                };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.into_inner().write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}/v1"), hits)
    }

    fn client_for(base_url: String) -> DecisionClient {
        let mut cfg = ProviderConfig::new(ProviderKind::OpenAi);
        cfg.base_url = base_url;
        cfg.api_key = Some("sk-test".into());
        cfg.timeout_ms = 1_000;
        DecisionClient::from_client(
            Client::new(cfg).unwrap(),
            "microsoft/microsoft-decision-1.1",
        )
    }

    #[test]
    fn decision_client_parses_live_response_fails_open_on_error_and_respects_cancel() {
        let (url_ok, hits_ok) = spawn_mock_server(
            200,
            r#"{"lockout_or_destructive": 0.71, "caution_change": 0.24, "routine_safe": 0.05}"#,
        );
        let dc = client_for(url_ok);
        let cancel = Cancel::default();
        let adv = dc
            .assess_command_risk("terminal_send", "reload", Some("cisco"), &cancel)
            .unwrap();
        assert_eq!(adv.label, "risco alto");
        assert_eq!(adv.confidence_pct, 71);
        assert_eq!(hits_ok.load(Ordering::SeqCst), 1);

        // Pre-cancelled token makes zero network calls
        let cancelled = Cancel::default();
        cancelled.cancel();
        assert!(dc
            .assess_command_risk("terminal_send", "reload", Some("cisco"), &cancelled)
            .is_none());
        assert_eq!(hits_ok.load(Ordering::SeqCst), 1);

        // HTTP 500 fails open silently and falls back to heuristic_effort
        let (url_err, hits_err) = spawn_mock_server(500, "");
        let dc_err = client_for(url_err);
        assert_eq!(
            dc_err.decide_effort("depure o bug BGP", "", &cancel),
            "high"
        );
        assert_eq!(dc_err.suggest_extra_tool_group("oi", &cancel), None);
        assert_eq!(hits_err.load(Ordering::SeqCst), 2);
    }
}
