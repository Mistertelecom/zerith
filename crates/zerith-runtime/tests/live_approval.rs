//! The approval loop against a real OpenAI-compatible endpoint. Ignored by
//! default: it needs a running server and spends tokens.
//!
//! ```text
//! ZERITH_LIVE_URL=http://localhost:11434/v1 ZERITH_LIVE_MODEL=<id> \
//!   cargo test -p zerith-runtime --test live_approval -- --ignored --nocapture
//! ```

use zerith_providers::{Cancel, Client, ProviderConfig, ProviderKind};
use zerith_runtime::{approval_channel, run_agent_turn, AgentOptions, Approval, RunEvent};
use zerith_security::{preset, Mode, Policy};
use zerith_sessions::{SessionSource, Store};
use zerith_tools::Registry;

#[test]
#[ignore = "needs ZERITH_LIVE_URL and ZERITH_LIVE_MODEL"]
fn a_real_model_asks_to_write_and_the_approval_lets_it() {
    let (Ok(url), Ok(model)) = (
        std::env::var("ZERITH_LIVE_URL"),
        std::env::var("ZERITH_LIVE_MODEL"),
    ) else {
        eprintln!("skipped: set ZERITH_LIVE_URL and ZERITH_LIVE_MODEL");
        return;
    };
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::Builder::new().tempdir_in(root).unwrap();

    // The keyless kind speaks the same OpenAI-compatible protocol and does
    // not refuse to start without a key; the key, when there is one, is sent.
    let mut cfg = ProviderConfig::new(ProviderKind::Ollama);
    cfg.base_url = url;
    cfg.api_key = zerith_core::secrets::resolve_api_key("openai", None)
        .ok()
        .flatten();
    let client = Client::new(cfg).unwrap();
    let store = Store::open_in_memory().unwrap();
    let sid = store
        .create_session("live", SessionSource::Native)
        .unwrap()
        .id;
    // workspace-write in Default mode: a write needs the user's yes.
    let policy = Policy::new(
        preset("workspace-write").unwrap(),
        Mode::Default,
        dir.path(),
    );
    let (tx, gate) = approval_channel();
    let reg = Registry::new();
    let mut asked = None;
    let mut tools = Vec::new();

    let report = run_agent_turn(
        &store,
        &client,
        Default::default(),
        AgentOptions {
            tools: Some(&reg),
            policy: Some(&policy),
            approvals: Some(&gate),
            max_steps: 6,
            ..Default::default()
        },
        &sid,
        &model,
        "Use a ferramenta write_file para criar o arquivo ola.txt com o conteúdo \
         exatamente: zerith. Não pergunte nada, só chame a ferramenta.",
        &Cancel::default(),
        |e| match e {
            RunEvent::ApprovalNeeded { id, tool, summary } => {
                eprintln!("aprovação pedida: {tool} — {summary}");
                asked = Some(tool);
                tx.answer(id, Approval::AllowOnce);
            }
            RunEvent::ToolCall { name, args, .. } => {
                eprintln!("ferramenta: {name} {args}");
                tools.push(name);
            }
            RunEvent::ToolResult { name, ok, .. } => eprintln!("resultado: {name} ok={ok}"),
            _ => {}
        },
    )
    .unwrap();
    eprintln!("resposta: {}", report.text);

    assert!(tools.iter().any(|t| t == "write_file"), "tools: {tools:?}");
    assert_eq!(asked.as_deref(), Some("write_file"));
    let written = std::fs::read_to_string(dir.path().join("ola.txt")).unwrap();
    assert_eq!(written.trim(), "zerith");
}
