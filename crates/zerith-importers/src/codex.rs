//! Importar do Codex: sessões (`~/.codex/sessions/YYYY/MM/DD/*.jsonl`) e
//! configuração (`AGENTS.md`, `config.toml [mcp_servers]`, skills, memórias).
//! Usa os tipos de plano de [`crate::claude_code`].
//!
//! Ideias (nenhum código copiado): Goose `session/import_formats/codex.rs`
//! (aaif-goose/goose, Apache-2.0) e Codex
//! `app-server/src/external_agent_migration/session_importer.rs`
//! (openai/codex, Apache-2.0).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use zerith_core::Paths;
use zerith_sessions::{NewMessage, Role};

use crate::claude_code::{
    parse_ts, plan_instructions, plan_sessions, push_assistant, title_from_user_text, tool_message,
    Item, Options, Origin, ParsedSession, CODEX_MARKER,
};

/// Onde estão os arquivos do Codex.
#[derive(Debug, Clone)]
pub struct CodexSource {
    /// `CODEX_HOME` ou `~/.codex`.
    pub home: PathBuf,
    /// Pasta do projeto atual (`.agents/skills`).
    pub project: Option<PathBuf>,
}

impl CodexSource {
    pub fn detect(project: Option<PathBuf>) -> Option<Self> {
        let home = match std::env::var_os("CODEX_HOME") {
            Some(d) => PathBuf::from(d),
            None => crate::claude_code::home_dir()?.join(".codex"),
        };
        home.is_dir().then_some(CodexSource { home, project })
    }
}

/// O plano completo do Codex.
pub fn plan(src: &CodexSource, paths: &Paths, opt: &Options) -> Vec<Item> {
    let mut items = Vec::new();
    if opt.sessions {
        let files = codex_session_files(&src.home);
        items.extend(plan_sessions(Origin::Codex, files, paths, opt));
    }
    if opt.config {
        items.extend(plan_instructions(
            &src.home.join("AGENTS.md"),
            CODEX_MARKER,
            paths,
        ));
    }
    items
}

/// `sessions/YYYY/MM/DD/*.jsonl`.
pub fn codex_session_files(home: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let Ok(ty) = e.file_type() else { continue };
            let p = e.path();
            if ty.is_dir() && depth < 3 {
                walk(&p, depth + 1, out);
            } else if ty.is_file() && depth == 3 && p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&home.join("sessions"), 0, &mut out);
    out
}

/// Primeira mensagem de usuário injetada pelo próprio Codex (contexto do
/// ambiente, AGENTS.md): fica no histórico mas não serve de título.
fn is_context_blob(text: &str) -> bool {
    let t = text.trim_start();
    [
        "<environment_context>",
        "<user_instructions>",
        "<app-context>",
        "<permissions instructions>",
        "# AGENTS.md",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => {
                    b.get("text").and_then(Value::as_str).map(str::to_string)
                }
                Some("input_image") => Some("[imagem omitida]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn output_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        // Versões novas: `{"content": "...", "success": bool}` ou lista de blocos.
        Some(Value::Object(o)) => match o.get("content") {
            Some(Value::String(s)) => s.clone(),
            c @ Some(Value::Array(_)) => content_text(c),
            _ => Value::Object(o.clone()).to_string(),
        },
        c @ Some(Value::Array(_)) => content_text(c),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Converte um rollout `.jsonl` do Codex. A primeira linha tem de ser
/// `session_meta`.
///
/// `response_item`: `message` de developer/system descartada; user/assistant
/// → texto; `reasoning` → raciocínio (resumo); `function_call` /
/// `custom_tool_call` → `tool_calls`; `*_output` → mensagem `Tool` pareada;
/// `web_search_call` → chamada sintética `web_search` com id
/// `codex_websearch_<linha>` e um resultado com o status. Tokens: soma de
/// `event_msg` `token_count.info.last_token_usage` (ou `payload.usage`).
pub fn parse_codex_session(text: &str, path: &Path) -> Option<ParsedSession> {
    let mut lines = text.lines().enumerate();
    let (_, first) = lines.next()?;
    let meta: Value = serde_json::from_str(first.trim()).ok()?;
    if meta.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let mp = meta.get("payload")?;
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let mut p = ParsedSession {
        id: s(mp, "id")
            .or_else(|| s(mp, "session_id"))
            .or_else(|| path.file_stem().map(|f| f.to_string_lossy().into_owned()))?,
        cwd: s(mp, "cwd"),
        ..ParsedSession::default()
    };
    let start = s(mp, "timestamp")
        .or_else(|| s(&meta, "timestamp"))
        .and_then(|t| parse_ts(&t))
        .unwrap_or(0);
    p.created_at = start;
    p.updated_at = start;
    let mut first_user = None;
    let mut names: std::collections::HashMap<String, String> = Default::default();
    let mut merge = false;
    for (no, line) in lines {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let ts = s(&v, "timestamp").and_then(|t| parse_ts(&t)).unwrap_or(0);
        if ts > 0 {
            if p.created_at == 0 {
                p.created_at = ts;
            }
            p.updated_at = p.updated_at.max(ts);
        }
        let Some(pl) = v.get("payload") else { continue };
        let pt = pl.get("type").and_then(Value::as_str).unwrap_or("");
        match v.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if p.model.is_none() {
                    p.model = s(pl, "model");
                }
                continue;
            }
            Some("event_msg") => {
                let usage = pl
                    .get("info")
                    .and_then(|i| i.get("last_token_usage"))
                    .or_else(|| pl.get("usage"));
                if let Some(u) = usage {
                    let n = |k: &str| u.get(k).and_then(Value::as_i64).unwrap_or(0);
                    // `input_tokens` do Codex já inclui o cache.
                    let cached = n("cached_input_tokens");
                    p.input_tokens += n("input_tokens") - cached;
                    p.cache_read_tokens += cached;
                    p.output_tokens += n("output_tokens");
                }
                continue;
            }
            Some("response_item") => {}
            _ => continue,
        }
        match pt {
            "message" => {
                let role = pl.get("role").and_then(Value::as_str).unwrap_or("");
                let text = content_text(pl.get("content"));
                match role {
                    "user" if !text.trim().is_empty() => {
                        if first_user.is_none() && !is_context_blob(&text) {
                            first_user = title_from_user_text(&text);
                        }
                        p.messages.push((NewMessage::new(Role::User, text), ts));
                        merge = false;
                    }
                    "assistant" => {
                        push_assistant(&mut p.messages, merge, ts, vec![text], vec![], vec![], 0);
                        merge = true;
                    }
                    _ => {} // developer/system: prompt do harness
                }
            }
            "reasoning" => {
                let summary: Vec<String> = pl
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                if !summary.is_empty() {
                    push_assistant(&mut p.messages, merge, ts, vec![], summary, vec![], 0);
                    merge = true;
                }
            }
            "function_call" | "custom_tool_call" | "local_shell_call" => {
                let id = s(pl, "call_id").or_else(|| s(pl, "id")).unwrap_or_default();
                let name = s(pl, "name").unwrap_or_else(|| "shell".into());
                let args = match (pl.get("arguments"), pl.get("input"), pl.get("action")) {
                    (Some(Value::String(a)), _, _) => a.clone(),
                    (Some(a), _, _) => a.to_string(),
                    (None, Some(Value::String(i)), _) => json!({ "input": i }).to_string(),
                    (None, _, Some(a)) => a.to_string(),
                    _ => "{}".to_string(),
                };
                names.insert(id.clone(), name.clone());
                let call = json!({ "id": id, "name": name, "arguments": args });
                push_assistant(&mut p.messages, merge, ts, vec![], vec![], vec![call], 0);
                merge = true;
            }
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                let id = s(pl, "call_id").unwrap_or_default();
                let out = output_text(pl.get("output"));
                let err = pl
                    .get("output")
                    .and_then(|o| o.get("success"))
                    .and_then(Value::as_bool)
                    == Some(false);
                p.messages
                    .push((tool_message(&id, names.get(&id), out, err), ts));
                merge = false;
            }
            "web_search_call" => {
                let id = format!("codex_websearch_{no}");
                let action = pl.get("action");
                let mut args = serde_json::Map::new();
                for k in ["query", "url"] {
                    if let Some(x) = action.and_then(|a| a.get(k)).and_then(Value::as_str) {
                        args.insert(k.into(), json!(x));
                    }
                }
                let call = json!({
                    "id": id,
                    "name": "web_search",
                    "arguments": Value::Object(args).to_string(),
                });
                push_assistant(&mut p.messages, merge, ts, vec![], vec![], vec![call], 0);
                let status = s(pl, "status").unwrap_or_else(|| "completed".into());
                let name = "web_search".to_string();
                p.messages.push((
                    tool_message(&id, Some(&name), format!("[web_search {status}]"), false),
                    ts,
                ));
                merge = false;
            }
            _ => {}
        }
    }
    p.title = first_user.unwrap_or_else(|| "Sessão importada do Codex".to_string());
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_code::tests::{scratch, write};
    use crate::claude_code::{apply, Action, Kind};
    use zerith_sessions::{SessionSource, Store};

    pub(crate) const CX_SESSION: &str = r#"{"timestamp":"2026-11-20T10:00:00.000Z","type":"session_meta","payload":{"id":"0199-abc","timestamp":"2026-11-20T10:00:00.000Z","cwd":"/w/cx"}}
{"timestamp":"2026-11-20T10:00:00.100Z","type":"turn_context","payload":{"cwd":"/w/cx","model":"gpt-x"}}
{"timestamp":"2026-11-20T10:00:00.200Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>..."}]}}
{"timestamp":"2026-11-20T10:00:00.300Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/w/cx</cwd>\n</environment_context>"}]}}
{"timestamp":"2026-11-20T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"rode ls"}]}}
{"timestamp":"2026-11-20T10:00:02.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"preciso listar"}],"encrypted_content":"x"}}
{"timestamp":"2026-11-20T10:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"call_1"}}
{"timestamp":"2026-11-20T10:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"a.txt\n"}}
{"timestamp":"2026-11-20T10:00:05.000Z","type":"response_item","payload":{"type":"web_search_call","status":"completed","action":{"type":"search","query":"rust"}}}
{"timestamp":"2026-11-20T10:00:06.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":20}}}}
{"timestamp":"2026-11-20T10:00:07.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Há a.txt."}]}}
"#;

    #[test]
    fn codex_items_map_and_calls_pair_with_results() {
        let p = parse_codex_session(CX_SESSION, Path::new("x.jsonl")).unwrap();
        assert_eq!(p.id, "0199-abc");
        assert_eq!(p.title, "rode ls");
        assert_eq!(p.cwd.as_deref(), Some("/w/cx"));
        assert_eq!(p.model.as_deref(), Some("gpt-x"));
        use Role::*;
        let roles: Vec<Role> = p.messages.iter().map(|(m, _)| m.role).collect();
        assert_eq!(
            roles,
            [User, User, Assistant, Tool, Assistant, Tool, Assistant]
        );
        let a = &p.messages[2].0;
        assert_eq!(a.reasoning.as_deref(), Some("preciso listar"));
        let c = &a.tool_calls.as_ref().unwrap()[0];
        assert_eq!(
            (c["id"].as_str(), c["name"].as_str()),
            (Some("call_1"), Some("shell"))
        );
        assert_eq!(c["arguments"], r#"{"command":["ls"]}"#);
        assert_eq!(
            p.messages[3].0.tool_calls.as_ref().unwrap()["tool_call_id"],
            "call_1"
        );
        assert_eq!(p.messages[3].0.content, "a.txt\n");
        let ws = &p.messages[4].0.tool_calls.as_ref().unwrap()[0];
        assert_eq!(ws["id"], "codex_websearch_8");
        assert_eq!(ws["name"], "web_search");
        assert_eq!(p.messages[5].0.content, "[web_search completed]");
        assert_eq!(p.messages[6].0.content, "Há a.txt.");
        assert_eq!(
            (p.input_tokens, p.cache_read_tokens, p.output_tokens),
            (40, 60, 20)
        );
        assert!(parse_codex_session("{\"type\":\"x\"}", Path::new("y")).is_none());
    }

    #[test]
    fn codex_sessions_import_once_and_append_only_what_grew() {
        let t = scratch("cx-sessions");
        let home = t.join("codex");
        let file = home.join("sessions/2026/11/20/rollout-1.jsonl");
        write(&file, CX_SESSION);
        let paths = Paths::with_root(t.join("zerith"));
        let src = CodexSource {
            home,
            project: None,
        };
        let opt = Options {
            config: false,
            visible: true,
            ..Options::default()
        };
        let p1 = plan(&src, &paths, &opt);
        assert_eq!(p1[0].action, Action::Import);
        let r = apply(&p1, &paths);
        assert_eq!(r.counts[&Kind::Session].imported, 1, "{:?}", r.errors);
        let store = Store::open(paths.db()).unwrap();
        let s = store.session("codex-0199-abc").unwrap().unwrap();
        assert!(!s.archived, "--visible");
        assert_eq!(s.source, SessionSource::Codex);
        assert_eq!(store.message_count(&s.id).unwrap(), 7);
        assert_eq!(plan(&src, &paths, &opt)[0].action, Action::Skip);
        let grown = format!(
            "{CX_SESSION}{}\n",
            r#"{"timestamp":"2026-11-21T10:00:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"valeu"}]}}"#
        );
        write(&file, &grown);
        let p3 = plan(&src, &paths, &opt);
        assert_eq!(p3[0].action, Action::Append);
        apply(&p3, &paths);
        apply(&plan(&src, &paths, &opt), &paths);
        assert_eq!(store.message_count(&s.id).unwrap(), 8);
        let _ = std::fs::remove_dir_all(&t);
    }
}
