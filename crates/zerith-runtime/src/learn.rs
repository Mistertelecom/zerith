//! Learning after a turn: memory and skills, only when something happened.
//!
//! Hermes reviews on a clock (every N tool calls / user turns) and tells the
//! reviewer that doing nothing is a missed opportunity; most of what that
//! writes to memory is noise. Here the trigger is the turn itself:
//!
//! 1. **Gate** ([`gate`]): cheap text and counter checks. A turn with no user
//!    correction/preference/personal fact and no real work (many tool calls,
//!    a failure that was recovered, a long multi-step run) costs nothing.
//! 2. **Review** ([`Reviewer`]): one model call with a compact digest, the
//!    existing facts (with ids) and the AI-created skills. Strict JSON back;
//!    "nothing to save" is the expected common answer.
//! 3. **Apply** ([`apply`]): Mem0-style ADD/UPDATE/NOOP against what is
//!    already stored (no duplicates), ACE-style deltas appended to an existing
//!    AI skill instead of a rewrite, and the ownership guard: only skills the
//!    model created may be patched.

use std::collections::HashSet;

use serde_json::{json, Value};
use zerith_core::Settings;
use zerith_memory::{Confidence, FactKind, MemoryBackend, Scope};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client, ProviderConfig};
use zerith_sessions::{NewUsageRecord, Store};
use zerith_skills::author::{leaks_private_data, NewSkill, MAX_BODY_CHARS};
use zerith_skills::{Library, Status};

use crate::RunEvent;

// ponytail: fixed thresholds picked by hand; tune from `usage` rows with
// role_kind = 'learn' (how often the gate opens vs. how often it writes).
/// Tool calls in one turn that count as real work.
pub const MIN_TOOL_CALLS: usize = 5;
/// Model requests in one turn that count as a long multi-step run.
pub const MIN_STEPS: usize = 6;
/// Characters of the turn sent to the reviewer, at most.
pub const DIGEST_CHARS: usize = 6000;
/// Memory writes one review may make.
pub const MAX_MEMORY_OPS: usize = 5;
/// Longest fact the reviewer may store.
const MAX_FACT_CHARS: usize = 300;
/// Longest skill description.
const MAX_DESC_CHARS: usize = 160;

/// Phrases that mark a correction, a preference, a rule or a personal fact.
/// Matched on whole words of the lowercased message.
// ponytail: a phrase list, not a classifier; a false hit costs one cheap
// review that answers "nothing", a miss is covered by the `memory_save` tool.
const USER_MARKERS: &[&str] = &[
    // pt
    "não faça",
    "não use",
    "não quero",
    "não gosto",
    "está errado",
    "está errada",
    "tá errado",
    "você errou",
    "na verdade",
    "prefiro",
    "sempre use",
    "sempre faça",
    "nunca use",
    "nunca faça",
    "pare de",
    "lembre",
    "lembra",
    "lembre-se",
    "meu",
    "minha",
    "meus",
    "minhas",
    "me chamo",
    "eu sou",
    "eu uso",
    "eu moro",
    "eu trabalho",
    "daqui pra frente",
    "de agora em diante",
    // en
    "don't",
    "do not",
    "always",
    "never",
    "i prefer",
    "actually",
    "remember",
    "stop",
    "wrong",
    "my",
    "i am",
    "i'm",
    "i use",
    "from now on",
];

/// One tool call as the reviewer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolTrace {
    pub name: String,
    pub ok: bool,
    /// Start of the output, for failures only (already redacted).
    pub error: String,
}

impl ToolTrace {
    /// Refused by this session's permission policy, not a failure of the
    /// tool: what was allowed here says nothing about the next session.
    pub fn denied(&self) -> bool {
        !self.ok && self.error.starts_with("negado")
    }
}

/// What a turn did, collected from its events. Cheap to build while the turn
/// streams; the gate and the review only read it.
#[derive(Debug, Clone, Default)]
pub struct TurnDigest {
    pub user: String,
    pub answer: String,
    pub tools: Vec<ToolTrace>,
    pub steps: usize,
    /// Skills loaded through the `skill` tool this turn.
    pub skills_used: Vec<String>,
}

impl TurnDigest {
    pub fn new(user: &str) -> Self {
        Self {
            user: user.to_string(),
            ..Self::default()
        }
    }

    /// Feeds one event of the turn. Only top-level events reach a host's
    /// callback (subagents report a summary), so this sees the turn itself.
    pub fn observe(&mut self, ev: &RunEvent) {
        match ev {
            RunEvent::Step { n } => self.steps = self.steps.max(*n),
            RunEvent::ToolCall { name, args, .. } if name == "skill" => {
                if let Some(s) = serde_json::from_str::<Value>(args)
                    .ok()
                    .and_then(|v| v["name"].as_str().map(str::to_string))
                {
                    if !self.skills_used.contains(&s) {
                        self.skills_used.push(s);
                    }
                }
            }
            RunEvent::ToolResult {
                name,
                ok,
                output_preview,
                ..
            } => self.tools.push(ToolTrace {
                name: name.clone(),
                ok: *ok,
                error: if *ok {
                    String::new()
                } else {
                    clip(&zerith_security::redact(output_preview), 200)
                },
            }),
            RunEvent::Steered { text } if !text.trim().is_empty() => {
                if !self.user.is_empty() {
                    self.user.push('\n');
                }
                self.user.push_str(text);
            }
            _ => {}
        }
    }
}

/// Why the gate opened. Empty means: no review, no cost.
pub fn gate(d: &TurnDigest) -> Vec<&'static str> {
    let mut why = Vec::new();
    if user_signal(&d.user) {
        why.push("usuário corrigiu, pediu ou contou algo sobre si");
    }
    if d.tools.len() >= MIN_TOOL_CALLS {
        why.push("muitas chamadas de ferramenta");
    }
    if recovered(&d.tools) {
        why.push("uma ferramenta falhou e depois deu certo");
    }
    if d.steps >= MIN_STEPS {
        why.push("turno longo com vários passos");
    }
    why
}

fn user_signal(text: &str) -> bool {
    let norm = format!(" {} ", words(text).join(" "));
    // A reply that opens with "não," / "no," is a correction.
    let opens_with_no = norm.starts_with(" não ") || norm.starts_with(" no ");
    opens_with_no
        || USER_MARKERS
            .iter()
            .any(|m| norm.contains(&format!(" {m} ")))
}

/// Lowercase words, apostrophes and inner hyphens kept ("don't", "lembre-se").
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .map(|w| w.trim_matches(|c| c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn recovered(tools: &[ToolTrace]) -> bool {
    tools
        .iter()
        .position(|t| !t.ok && !t.denied())
        .is_some_and(|i| tools[i + 1..].iter().any(|t| t.ok))
}

/// The one model call of the review. A trait so tests (and a host that wants
/// a different model) replace it.
pub trait Reviewer {
    fn review(&self, system: &str, digest: &str, cancel: &Cancel) -> Result<String, String>;
}

/// The default reviewer: one blocking request, recorded in `usage` with
/// `role_kind = 'learn'` when a store is given.
pub struct ClientReviewer<'a> {
    pub client: &'a Client,
    pub model: &'a str,
    pub record: Option<(&'a Store, &'a str)>,
}

impl Reviewer for ClientReviewer<'_> {
    fn review(&self, system: &str, digest: &str, cancel: &Cancel) -> Result<String, String> {
        let req = ChatRequest::new(
            self.model,
            vec![ChatMessage::system(system), ChatMessage::user(digest)],
        )
        .max_tokens(2000)
        .temperature(0.0);
        let t0 = std::time::Instant::now();
        let res = self.client.complete(&req, cancel);
        if let Some((store, session)) = self.record {
            let cfg = self.client.config();
            let provider = if cfg.name.trim().is_empty() {
                cfg.kind.as_str().to_string()
            } else {
                cfg.name.clone()
            };
            let usage = res.as_ref().ok().and_then(|c| c.usage.clone());
            let _ = store.record_usage(&NewUsageRecord {
                session_id: Some(session.to_string()),
                account_id: cfg
                    .account_id
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| provider.clone()),
                provider,
                model: self.model.to_string(),
                role_kind: "learn".into(),
                prompt_tokens: usage.as_ref().map_or(0, |u| u.prompt_tokens),
                completion_tokens: usage.as_ref().map_or(0, |u| u.completion_tokens),
                status: if res.is_ok() { "ok" } else { "error" }.into(),
                latency_ms: t0.elapsed().as_millis() as u64,
                ..NewUsageRecord::default()
            });
        }
        res.map(|c| c.text)
            .map_err(|e| zerith_providers::explain::headline(&e))
    }
}

/// The client and model the review uses: `[learning].model` when set, else
/// the model the turn was routed to (first available candidate of its gateway,
/// falling back to the first candidate).
pub fn route(
    settings: &Settings,
    turn_model: &str,
    fallback: &ProviderConfig,
) -> Option<(Client, String)> {
    route_with_store(settings, turn_model, fallback, None)
}

/// Same as [`route`], preferring the first candidate whose account is available
/// according to `store` (quotas and cooldowns).
pub fn route_with_store(
    settings: &Settings,
    turn_model: &str,
    fallback: &ProviderConfig,
    store: Option<&Store>,
) -> Option<(Client, String)> {
    let own = settings.learning.model.trim();
    let wanted = if own.is_empty() { turn_model } else { own };
    let candidates = crate::resolve_gateway(settings, wanted, fallback)
        .0
        .candidates;
    let chosen = store
        .and_then(|st| {
            candidates.iter().find(|c| {
                st.is_account_available(
                    &c.account_id,
                    c.window_hours,
                    c.quota_tokens,
                    c.quota_requests,
                )
                .unwrap_or(true)
            })
        })
        .or_else(|| candidates.first())?
        .clone();
    Client::new(chosen.config).ok().map(|c| (c, chosen.model))
}

/// The fixed instruction of the review call.
pub const REVIEW_PROMPT: &str = r#"Você revisa um turno JÁ CONCLUÍDO entre um usuário e um agente e decide se há algo que valha guardar para conversas futuras. Responda SÓ com um objeto JSON, sem texto fora dele:
{"memory":[{"op":"add|update|none","id":123,"kind":"instruction|preference|fact","text":"..."}],"skill":{"op":"none|create|patch","name":"nome-em-kebab","description":"Use when ... (até 160 caracteres)","instructions":"markdown"}}

Regras:
- Não guardar nada é normal e é a resposta mais comum: {"memory":[],"skill":{"op":"none"}}. Na dúvida, não guarde.
- memory: só o que o USUÁRIO disse sobre si, como quer ser atendido, ou regras que deu. instruction = regra que o agente deve seguir; preference = gosto ou estilo; fact = fato pessoal duradouro. Uma frase curta e autônoma por item, na língua do usuário, em terceira pessoa ("O usuário prefere ...").
- Nunca guarde: estado ou progresso da tarefa, listas de arquivos, valores de uso único (ids, caminhos, versões, números), o conteúdo da resposta do agente, segredos, senhas, tokens, chaves.
- Sem duplicatas: se um item de "Memória existente" (#id) já diz o mesmo, não repita. Se o usuário corrigiu ou detalhou um item existente, use "update" com o id dele e o texto completo novo.
- skill: só para um procedimento reutilizável, armadilha ou correção descoberto PELO TRABALHO deste turno (por exemplo: um comando falhou e outro funcionou). Chamada "negada pela política desta sessão" não é lição: as permissões mudam de uma sessão para outra; nunca crie nem altere skill sobre o que foi negado ou permitido. Nome de classe do problema ("windows-path-quoting"), nunca número de PR, mensagem de erro literal ou nome de arquivo do usuário. A descrição começa com "Use when".
- Prefira "patch" numa skill de "Skills da IA" quando o assunto for o mesmo. Em patch, "instructions" traz SÓ as lições novas (tópicos curtos) que serão acrescentadas ao fim; não reescreva a skill.
- Em create, "instructions" é o corpo completo em markdown: quando usar, passos, armadilhas. Sem caminhos da pasta pessoal e sem segredos."#;

/// Where the review may write. `None` fields turn that half off.
pub struct Pass<'a> {
    pub memory: Option<(&'a dyn MemoryBackend, &'a Scope)>,
    pub skills: Option<&'a Library>,
    pub session_id: &'a str,
}

/// One thing learned, shaped for the window's "Aprendi:" note and its undo.
#[derive(Debug, Clone, PartialEq)]
pub struct Learned {
    /// `memória` or `skill`.
    pub kind: &'static str,
    /// `add`, `update`, `create` or `patch`.
    pub op: &'static str,
    pub text: String,
    pub id: Option<i64>,
    pub name: Option<String>,
    /// What an undo restores (old fact text, old skill body). `None` = delete.
    pub previous: Option<String>,
}

impl Learned {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind, "op": self.op, "text": self.text,
            "id": self.id, "name": self.name, "previous": self.previous,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// Why the gate opened; empty = no review was made.
    pub signals: Vec<&'static str>,
    pub reviewed: bool,
    pub learned: Vec<Learned>,
    /// Ops refused (ownership, private data, invalid), for logs and tests.
    pub refused: Vec<String>,
    pub error: Option<String>,
}

/// Gate, review, apply. Never fails the caller: errors land in `Outcome`.
pub fn after_turn(
    digest: &TurnDigest,
    pass: &Pass,
    reviewer: &dyn Reviewer,
    cancel: &Cancel,
) -> Outcome {
    let mut out = Outcome {
        signals: gate(digest),
        ..Outcome::default()
    };
    if out.signals.is_empty() || (pass.memory.is_none() && pass.skills.is_none()) {
        return out;
    }
    let (prompt, shown) = render(digest, &out.signals, pass);
    out.reviewed = true;
    let text = match reviewer.review(REVIEW_PROMPT, &prompt, cancel) {
        Ok(t) => t,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    let Some(review) = parse_review(&text) else {
        out.error = Some("revisão sem JSON válido".into());
        return out;
    };
    apply(&review, pass, &shown, &mut out);
    out
}

/// A memory write the reviewer asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum MemOp {
    Add { kind: FactKind, text: String },
    Update { id: i64, text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillOp {
    Create {
        name: String,
        description: String,
        instructions: String,
    },
    Patch {
        name: String,
        instructions: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Review {
    pub memory: Vec<MemOp>,
    pub skill: Option<SkillOp>,
}

/// Reads the reviewer's answer. Code fences and prose around the object are
/// tolerated; anything that is not the expected JSON is `None` (a no-op).
pub fn parse_review(text: &str) -> Option<Review> {
    let (a, b) = (text.find('{')?, text.rfind('}')?);
    let v: Value = serde_json::from_str(text.get(a..=b)?).ok()?;
    if !v.is_object() {
        return None;
    }
    let s = |x: &Value| x.as_str().unwrap_or("").trim().to_string();
    let mut r = Review::default();
    for m in v["memory"].as_array().into_iter().flatten() {
        let text = s(&m["text"]);
        let kind = match m["kind"].as_str().unwrap_or("fact") {
            "instruction" => FactKind::Instruction,
            "preference" => FactKind::Preference,
            _ => FactKind::Fact,
        };
        match m["op"].as_str().unwrap_or("") {
            "add" if !text.is_empty() => r.memory.push(MemOp::Add { kind, text }),
            "update" if !text.is_empty() => match m["id"].as_i64() {
                Some(id) => r.memory.push(MemOp::Update { id, text }),
                None => r.memory.push(MemOp::Add { kind, text }),
            },
            _ => {}
        }
    }
    let sk = &v["skill"];
    let (name, instructions) = (s(&sk["name"]), s(&sk["instructions"]));
    r.skill = match sk["op"].as_str().unwrap_or("none") {
        "create" if !name.is_empty() && !instructions.is_empty() => Some(SkillOp::Create {
            name,
            description: s(&sk["description"]),
            instructions,
        }),
        "patch" if !name.is_empty() && !instructions.is_empty() => {
            Some(SkillOp::Patch { name, instructions })
        }
        _ => None,
    };
    Some(r)
}

/// The digest the reviewer reads, and the fact ids it was shown (an update
/// may only target one of those).
fn render(d: &TurnDigest, signals: &[&str], pass: &Pass) -> (String, HashSet<i64>) {
    let mut turn = format!(
        "# Turno\n## Mensagem do usuário\n{}\n\n## Resposta final do agente\n{}\n",
        clip(&d.user, 1500),
        clip(&d.answer, 1500)
    );
    if !d.tools.is_empty() {
        turn.push_str(&format!("\n## Ferramentas ({} chamadas)\n", d.tools.len()));
        for t in d.tools.iter().take(40) {
            if t.ok {
                turn.push_str(&format!("- {}: ok\n", t.name));
            } else if t.denied() {
                turn.push_str(&format!(
                    "- {}: negado pela política desta sessão\n",
                    t.name
                ));
            } else {
                turn.push_str(&format!(
                    "- {}: ERRO: {}\n",
                    t.name,
                    t.error.replace('\n', " ")
                ));
            }
        }
    }
    turn.push_str(&format!(
        "\n## Por que revisar\n- {}\n",
        signals.join("\n- ")
    ));
    let mut out = clip(&turn, DIGEST_CHARS);

    let mut shown = HashSet::new();
    if let Some((mem, scope)) = pass.memory {
        let mut facts: Vec<_> = mem
            .list(scope, 50)
            .unwrap_or_default()
            .into_iter()
            .filter(|f| matches!(f.kind, FactKind::Instruction | FactKind::Preference))
            .collect();
        facts.extend(
            mem.recall(scope, &d.user, 10)
                .unwrap_or_default()
                .into_iter()
                .map(|h| h.fact),
        );
        out.push_str("\n# Memória existente\n");
        let mut any = false;
        for f in facts
            .into_iter()
            .filter(|f| f.superseded_by.is_none())
            .take(40)
        {
            if shown.insert(f.id) {
                out.push_str(&format!(
                    "#{} [{}] {}\n",
                    f.id,
                    f.kind.as_str(),
                    clip(&f.text, 200)
                ));
                any = true;
            }
        }
        if !any {
            out.push_str("(vazia)\n");
        }
    }
    if let Some(lib) = pass.skills {
        let rows = lib.manifest().map(|m| m.skills).unwrap_or_default();
        let used: Vec<_> = rows
            .iter()
            .filter(|s| d.skills_used.contains(&s.name))
            .collect();
        if !used.is_empty() {
            out.push_str("\n# Skills usadas neste turno\n");
            for s in &used {
                out.push_str(&format!("- {}: {}\n", s.name, clip(&s.description, 160)));
            }
        }
        let ai: Vec<_> = rows
            .iter()
            .filter(|s| s.source.starts_with(zerith_skills::author::AI_SOURCE))
            .take(20)
            .collect();
        out.push_str("\n# Skills da IA (podem receber patch)\n");
        if ai.is_empty() {
            out.push_str("(nenhuma)\n");
        }
        for s in ai {
            out.push_str(&format!("- {}: {}\n", s.name, clip(&s.description, 160)));
            if d.skills_used.contains(&s.name) {
                if let Ok(body) = lib.load_body(&s.name) {
                    out.push_str(&format!(
                        "  corpo atual:\n{}\n",
                        clip(skill_body(&body), 1200)
                    ));
                }
            }
        }
    }
    (out, shown)
}

/// Applies a parsed review. Public so a host can re-apply a review it got
/// some other way; `shown` are the fact ids the reviewer saw.
pub fn apply(review: &Review, pass: &Pass, shown: &HashSet<i64>, out: &mut Outcome) {
    if let Some((mem, scope)) = pass.memory {
        let allowed = mem.is_extraction_allowed(pass.session_id).unwrap_or(false);
        for op in review.memory.iter().take(MAX_MEMORY_OPS) {
            if !allowed {
                out.refused
                    .push("memória pausada ou conversa excluída".into());
                break;
            }
            apply_memory(op, mem, scope, pass.session_id, shown, out);
        }
    }
    if let (Some(lib), Some(op)) = (pass.skills, &review.skill) {
        apply_skill(op, lib, pass.session_id, out);
    }
}

fn apply_memory(
    op: &MemOp,
    mem: &dyn MemoryBackend,
    scope: &Scope,
    session: &str,
    shown: &HashSet<i64>,
    out: &mut Outcome,
) {
    let text = match op {
        MemOp::Add { text, .. } | MemOp::Update { text, .. } => text.trim(),
    };
    if text.chars().count() < 4 || text.chars().count() > MAX_FACT_CHARS {
        out.refused
            .push(format!("fato curto ou longo demais: {}", clip(text, 60)));
        return;
    }
    if let Some(why) = leaks_private_data(text) {
        out.refused.push(format!("fato {why}"));
        return;
    }
    let existing: Vec<_> = mem
        .list(scope, 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f.superseded_by.is_none())
        .collect();
    let target = match op {
        MemOp::Update { id, .. } if shown.contains(id) => Some(*id),
        _ => None,
    };
    // NOOP: the same thing is already stored (other than the fact being updated).
    if existing
        .iter()
        .any(|f| Some(f.id) != target && near_duplicate(&f.text, text))
    {
        return;
    }
    match (op, target) {
        (MemOp::Update { .. }, Some(id)) => {
            let Some(old) = existing.iter().find(|f| f.id == id) else {
                return;
            };
            if near_duplicate(&old.text, text) {
                return;
            }
            if mem.update(id, text).is_ok() {
                out.learned.push(Learned {
                    kind: "memória",
                    op: "update",
                    text: text.to_string(),
                    id: Some(id),
                    name: None,
                    previous: Some(old.text.clone()),
                });
            }
        }
        _ => {
            let kind = match op {
                MemOp::Add { kind, .. } => *kind,
                MemOp::Update { .. } => FactKind::Fact,
            };
            let source = format!("learn:{session}");
            if let Ok(id) = mem.remember(scope, kind, text, Confidence::Inferred, Some(&source)) {
                out.learned.push(Learned {
                    kind: "memória",
                    op: "add",
                    text: text.to_string(),
                    id: Some(id),
                    name: None,
                    previous: None,
                });
            }
        }
    }
}

fn apply_skill(op: &SkillOp, lib: &Library, session: &str, out: &mut Outcome) {
    let rows = lib.manifest().map(|m| m.skills).unwrap_or_default();
    let owner = |name: &str| {
        rows.iter()
            .find(|s| s.name == name)
            .map(|s| s.source.starts_with(zerith_skills::author::AI_SOURCE))
    };
    // A create on a name the model already owns is a patch (ACE: grow the
    // playbook, never fork it).
    let op = match op {
        SkillOp::Create {
            name, instructions, ..
        } if owner(name) == Some(true) => SkillOp::Patch {
            name: name.clone(),
            instructions: instructions.clone(),
        },
        other => other.clone(),
    };
    match op {
        SkillOp::Create {
            name,
            description,
            instructions,
        } => {
            if owner(&name) == Some(false) {
                out.refused.push(format!(
                    "{name}: já existe uma skill da pessoa com esse nome"
                ));
                return;
            }
            let mut d = description.trim().replace(['\n', '\r'], " ");
            if !d.to_lowercase().starts_with("use ") {
                d = format!("Use when {d}");
            }
            let new = NewSkill {
                name: name.clone(),
                description: clip(&d, MAX_DESC_CHARS),
                instructions,
                files: Vec::new(),
            };
            match lib.create_ai_skill(&new, session) {
                Ok(created) => {
                    // Enabled only when the scan had nothing to say; it stays
                    // marked as the model's, so the person can review or undo.
                    if created.notes.is_empty() {
                        let _ = lib.set_status(&created.name, Status::Enabled);
                    }
                    out.learned.push(Learned {
                        kind: "skill",
                        op: "create",
                        text: new.description.clone(),
                        id: None,
                        name: Some(created.name),
                        previous: None,
                    });
                }
                Err(e) => out.refused.push(format!("{name}: {e}")),
            }
        }
        SkillOp::Patch { name, instructions } => {
            if owner(&name) != Some(true) {
                out.refused.push(format!(
                    "{name}: só skills criadas pela IA recebem patch automático"
                ));
                return;
            }
            if let Some(why) = leaks_private_data(&instructions) {
                out.refused.push(format!("{name}: o patch {why}"));
                return;
            }
            let Ok(full) = lib.load_body(&name) else {
                return;
            };
            let body = skill_body(&full).trim_end().to_string();
            // Line-level dedupe: only lessons not already in the skill.
            let have: HashSet<String> = body.lines().map(norm).filter(|l| !l.is_empty()).collect();
            let fresh: Vec<&str> = instructions
                .lines()
                .filter(|l| {
                    let n = norm(l);
                    n.is_empty() || !have.contains(&n)
                })
                .collect();
            if fresh.iter().all(|l| norm(l).is_empty()) {
                return;
            }
            let new_body = format!("{body}\n\n{}", fresh.join("\n").trim());
            if new_body.chars().count() > MAX_BODY_CHARS {
                out.refused
                    .push(format!("{name}: a skill ficaria grande demais"));
                return;
            }
            match lib.update_ai_skill(&name, &new_body) {
                Ok(()) => out.learned.push(Learned {
                    kind: "skill",
                    op: "patch",
                    text: clip(fresh.join(" ").trim(), 200),
                    id: None,
                    name: Some(name),
                    previous: Some(body),
                }),
                Err(e) => out.refused.push(format!("{name}: {e}")),
            }
        }
    }
}

/// Undoes one [`Learned`] item: restores `previous`, or deletes what was
/// created. Skills go through the ownership-checked author API.
pub fn undo(
    item_kind: &str,
    id: Option<i64>,
    name: Option<&str>,
    previous: Option<&str>,
    memory: Option<&dyn MemoryBackend>,
    skills: Option<&Library>,
) -> Result<(), String> {
    match (item_kind, id, name) {
        ("memória", Some(id), _) => {
            let mem = memory.ok_or("memória indisponível")?;
            match previous {
                Some(p) => mem.update(id, p),
                None => mem.forget(id),
            }
            .map_err(|e| e.to_string())
        }
        ("skill", _, Some(name)) => {
            let lib = skills.ok_or("skills indisponíveis")?;
            match previous {
                Some(p) => lib.update_ai_skill(name, p),
                None => lib.delete_ai_skill(name),
            }
            .map_err(|e| e.to_string())
        }
        _ => Err("item de aprendizado inválido".into()),
    }
}

/// The body of a `SKILL.md` (what follows the frontmatter).
fn skill_body(full: &str) -> &str {
    zerith_skills::parse(full).map(|(_, b)| b).unwrap_or(full)
}

fn norm(s: &str) -> String {
    words(s)
        .into_iter()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Same fact in other words: equal after normalizing, or one contains the
/// other and they are within 20% in length.
fn near_duplicate(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    a == b || (long.contains(short.as_str()) && short.len() * 10 >= long.len() * 8)
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    struct Canned {
        calls: Cell<usize>,
        answer: String,
        seen: RefCell<String>,
    }

    impl Canned {
        fn new(answer: &str) -> Self {
            Self {
                calls: Cell::new(0),
                answer: answer.into(),
                seen: RefCell::new(String::new()),
            }
        }
    }

    impl Reviewer for Canned {
        fn review(&self, _s: &str, digest: &str, _c: &Cancel) -> Result<String, String> {
            self.calls.set(self.calls.get() + 1);
            *self.seen.borrow_mut() = digest.to_string();
            Ok(self.answer.clone())
        }
    }

    fn lib(tag: &str) -> Library {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/test-scratch/learn-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        Library::open(root).unwrap()
    }

    fn tool(name: &str, ok: bool) -> ToolTrace {
        ToolTrace {
            name: name.into(),
            ok,
            error: if ok { String::new() } else { "exit 1".into() },
        }
    }

    #[test]
    fn the_gate_opens_only_on_a_signal() {
        let plain = TurnDigest::new("qual a capital da França?");
        assert!(gate(&plain).is_empty());
        assert!(gate(&TurnDigest::new("por que isso não compila?")).is_empty());
        for msg in [
            "prefiro respostas curtas",
            "na verdade o servidor é Debian",
            "Não, use tabs",
            "minha filha se chama Ana",
            "I prefer tabs",
            "don't use emojis",
            "pare de pedir confirmação",
            "está errado, o arquivo é outro",
            "nunca use print para log",
        ] {
            assert!(!gate(&TurnDigest::new(msg)).is_empty(), "{msg}");
        }
        // Describing the bug is not a correction of the assistant.
        for msg in [
            "diga o que estava errado no código",
            "o loop nunca termina, corrija",
        ] {
            assert!(gate(&TurnDigest::new(msg)).is_empty(), "{msg}");
        }
        let mut work = TurnDigest::new("rode os testes");
        work.tools = (0..MIN_TOOL_CALLS).map(|_| tool("shell", true)).collect();
        assert!(!gate(&work).is_empty());
        work.tools = vec![tool("shell", false), tool("shell", true)];
        assert_eq!(
            gate(&work),
            vec!["uma ferramenta falhou e depois deu certo"]
        );
        work.tools = vec![tool("shell", true), tool("shell", false)];
        assert!(
            gate(&work).is_empty(),
            "failure without recovery is no lesson"
        );
        // A call the session's policy refused, then another that ran: the
        // lesson would be about this session's permissions, not the work.
        let denied = ToolTrace {
            name: "shell".into(),
            ok: false,
            error: "negado: precisa de aprovação (shell: x > f) e não há quem aprove".into(),
        };
        work.tools = vec![denied.clone(), tool("shell", true)];
        assert!(
            gate(&work).is_empty(),
            "a refusal is not a recovered failure"
        );
        assert!(denied.denied() && !tool("shell", false).denied());
        work.tools.clear();
        work.steps = MIN_STEPS;
        assert!(!gate(&work).is_empty());
    }

    #[test]
    fn the_digest_follows_the_events() {
        let mut d = TurnDigest::new("x");
        d.observe(&RunEvent::Step { n: 3 });
        d.observe(&RunEvent::ToolCall {
            id: "1".into(),
            name: "skill".into(),
            args: r#"{"name":"git-rebase"}"#.into(),
        });
        d.observe(&RunEvent::ToolResult {
            id: "1".into(),
            name: "shell".into(),
            ok: false,
            output_preview: "exit 2\nboom".into(),
        });
        assert_eq!(d.steps, 3);
        assert_eq!(d.skills_used, vec!["git-rebase"]);
        assert_eq!(d.tools[0].error, "exit 2\nboom");
    }

    #[test]
    fn parsing_is_strict_about_shape_and_lenient_about_wrapping() {
        let fenced = "Aqui:\n```json\n{\"memory\":[{\"op\":\"add\",\"kind\":\"preference\",\"text\":\"O usuário prefere tabs\"},{\"op\":\"none\"},{\"op\":\"update\",\"id\":7,\"text\":\"novo\"}],\"skill\":{\"op\":\"patch\",\"name\":\"x\",\"instructions\":\"- a\"}}\n```";
        let r = parse_review(fenced).unwrap();
        assert_eq!(
            r.memory,
            vec![
                MemOp::Add {
                    kind: FactKind::Preference,
                    text: "O usuário prefere tabs".into()
                },
                MemOp::Update {
                    id: 7,
                    text: "novo".into()
                }
            ]
        );
        assert!(matches!(r.skill, Some(SkillOp::Patch { .. })));
        assert_eq!(
            parse_review(r#"{"memory":[],"skill":{"op":"none"}}"#),
            Some(Review::default())
        );
        assert_eq!(parse_review("nada a salvar"), None);
        assert_eq!(parse_review("{memory: [}"), None);
        assert_eq!(parse_review("[1,2]"), None);
    }

    #[test]
    fn a_turn_without_signal_never_calls_the_reviewer() {
        let mem = zerith_memory::SqliteMemory::open_in_memory().unwrap();
        let scope = Scope::user("t");
        let r = Canned::new(r#"{"memory":[{"op":"add","kind":"fact","text":"lixo"}]}"#);
        let pass = Pass {
            memory: Some((&mem, &scope)),
            skills: None,
            session_id: "s",
        };
        let out = after_turn(
            &TurnDigest::new("quanto é 2+2?"),
            &pass,
            &r,
            &Cancel::default(),
        );
        assert_eq!(r.calls.get(), 0);
        assert!(!out.reviewed && out.learned.is_empty());
        assert!(mem.list(&scope, 10).unwrap().is_empty());
    }

    #[test]
    fn memory_adds_dedupes_and_updates_only_shown_ids() {
        let mem = zerith_memory::SqliteMemory::open_in_memory().unwrap();
        let scope = Scope::user("t");
        let pass = Pass {
            memory: Some((&mem, &scope)),
            skills: None,
            session_id: "s",
        };
        let d = TurnDigest::new("prefiro respostas curtas e meu editor é o Helix");
        let r = Canned::new(
            r#"{"memory":[{"op":"add","kind":"preference","text":"O usuário prefere respostas curtas."},{"op":"add","kind":"fact","text":"O editor do usuário é o Helix"},{"op":"add","kind":"fact","text":"use a chave sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}],"skill":{"op":"none"}}"#,
        );
        let out = after_turn(&d, &pass, &r, &Cancel::default());
        assert_eq!(r.calls.get(), 1);
        assert_eq!(out.learned.len(), 2, "{out:?}");
        assert_eq!(out.refused.len(), 1, "secret refused: {out:?}");
        // Same review again: near-exact duplicates are a NOOP.
        let again = after_turn(&d, &pass, &r, &Cancel::default());
        assert!(again.learned.is_empty(), "{again:?}");
        assert_eq!(mem.list(&scope, 10).unwrap().len(), 2);
        assert!(r
            .seen
            .borrow()
            .contains("[preference] O usuário prefere respostas curtas."));

        let pref = out.learned[0].id.unwrap();
        let upd = Canned::new(&format!(
            r#"{{"memory":[{{"op":"update","id":{pref},"kind":"preference","text":"O usuário prefere respostas curtas, em tópicos"}},{{"op":"update","id":99999,"text":"O usuário mora em Natal"}}]}}"#
        ));
        let out = after_turn(
            &TurnDigest::new("na verdade, em tópicos"),
            &pass,
            &upd,
            &Cancel::default(),
        );
        assert_eq!(out.learned[0].op, "update");
        assert_eq!(
            out.learned[0].previous.as_deref(),
            Some("O usuário prefere respostas curtas.")
        );
        assert_eq!(out.learned[1].op, "add", "unknown id becomes an add");
        assert_eq!(
            mem.get(pref).unwrap().text,
            "O usuário prefere respostas curtas, em tópicos"
        );
        undo(
            "memória",
            Some(pref),
            None,
            out.learned[0].previous.as_deref(),
            Some(&mem),
            None,
        )
        .unwrap();
        assert_eq!(
            mem.get(pref).unwrap().text,
            "O usuário prefere respostas curtas."
        );
    }

    #[test]
    fn skills_are_created_enabled_patched_as_deltas_and_only_when_owned() {
        let l = lib("skills");
        let pass = Pass {
            memory: None,
            skills: Some(&l),
            session_id: "s1",
        };
        let mut d = TurnDigest::new("rode o build");
        d.tools = vec![tool("shell", false), tool("shell", true)];
        let create = Canned::new(
            r##"{"memory":[],"skill":{"op":"create","name":"windows-temp-linker","description":"Use when link.exe fails writing to Temp on Windows","instructions":"# Linker\n\n- Aponte TMP para dentro do repo."}}"##,
        );
        let out = after_turn(&d, &pass, &create, &Cancel::default());
        assert_eq!(out.learned.len(), 1, "{out:?}");
        let ai = l.ai_created().unwrap();
        assert_eq!(ai[0].status, Status::Enabled, "clean scan: enabled");

        // A second create on the same name grows the skill instead (ACE).
        let patch = Canned::new(
            r#"{"skill":{"op":"create","name":"windows-temp-linker","description":"x","instructions":"- Aponte TMP para dentro do repo.\n- Limpe target/tmp se o disco encher."}}"#,
        );
        let out = after_turn(&d, &pass, &patch, &Cancel::default());
        assert_eq!(out.learned[0].op, "patch", "{out:?}");
        let body = l.load_body("windows-temp-linker").unwrap();
        assert_eq!(
            body.matches("Aponte TMP").count(),
            1,
            "old lesson kept once: {body}"
        );
        assert!(body.contains("Limpe target/tmp"));
        // Same delta again: nothing new, no write.
        assert!(after_turn(&d, &pass, &patch, &Cancel::default())
            .learned
            .is_empty());

        // The person's own skill is never patched.
        let src = l.root().join("../mine-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: da-pessoa\ndescription: d\n---\ncorpo",
        )
        .unwrap();
        l.install(&src, "mine").unwrap();
        let foreign =
            Canned::new(r#"{"skill":{"op":"patch","name":"da-pessoa","instructions":"- x"}}"#);
        let out = after_turn(&d, &pass, &foreign, &Cancel::default());
        assert!(out.learned.is_empty());
        assert!(
            out.refused[0].contains("só skills criadas pela IA"),
            "{out:?}"
        );
        assert!(l.load_body("da-pessoa").unwrap().ends_with("corpo"));

        undo(
            "skill",
            None,
            Some("windows-temp-linker"),
            None,
            None,
            Some(&l),
        )
        .unwrap();
        assert!(l.ai_created().unwrap().is_empty());
        assert!(undo("skill", None, Some("da-pessoa"), None, None, Some(&l)).is_err());
    }

    #[test]
    fn invalid_json_is_a_no_op() {
        let mem = zerith_memory::SqliteMemory::open_in_memory().unwrap();
        let scope = Scope::user("t");
        let pass = Pass {
            memory: Some((&mem, &scope)),
            skills: None,
            session_id: "s",
        };
        let r = Canned::new("Claro! Vou salvar que você prefere tabs.");
        let out = after_turn(
            &TurnDigest::new("prefiro tabs"),
            &pass,
            &r,
            &Cancel::default(),
        );
        assert!(out.reviewed && out.learned.is_empty() && out.error.is_some());
        assert!(mem.list(&scope, 10).unwrap().is_empty());
    }

    #[test]
    fn steered_messages_append_to_digest_user_and_open_gate() {
        let mut d = TurnDigest::new("rode o build");
        assert!(gate(&d).is_empty());
        d.observe(&RunEvent::Steered {
            text: "na verdade prefiro tabs".into(),
        });
        assert_eq!(d.user, "rode o build\nna verdade prefiro tabs");
        assert!(
            !gate(&d).is_empty(),
            "steered correction must open the gate"
        );
    }

    #[test]
    fn route_with_store_skips_cooled_down_or_exhausted_account() {
        let store = Store::open_in_memory().unwrap();
        store
            .set_account_cooldown(
                "acc-1",
                zerith_sessions::migrations::now_ms() + 60_000,
                Some("429"),
            )
            .unwrap();
        let mut settings = Settings::default();
        settings.accounts.push(zerith_core::settings::AccountEntry {
            id: "acc-1".into(),
            provider: "openai".into(),
            base_url: Some("https://acc1.example/v1".into()),
            priority: 0,
            enabled: true,
            ..Default::default()
        });
        settings.accounts.push(zerith_core::settings::AccountEntry {
            id: "acc-2".into(),
            provider: "openai".into(),
            base_url: Some("https://acc2.example/v1".into()),
            priority: 1,
            enabled: true,
            ..Default::default()
        });
        let fb = ProviderConfig::new(zerith_providers::ProviderKind::OpenAi).with_key("k");
        let (client, model) = route_with_store(&settings, "gpt-4o", &fb, Some(&store)).unwrap();
        assert_eq!(model, "gpt-4o");
        assert_eq!(client.config().account_id.as_deref(), Some("acc-2"));
        assert_eq!(client.config().base_url, "https://acc2.example/v1");
    }
}
