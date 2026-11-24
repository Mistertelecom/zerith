//! Workflows do `<raiz>/workflows.db` no `zerith daemon`: a cada ciclo roda o
//! agendador compartilhado (`zerith_workflows::scheduler::tick`), o mesmo do
//! app desktop, para a VPS executar os workflows desenhados no desktop.

use std::sync::Arc;
use std::thread::JoinHandle;

use zerith_core::{Paths, Settings};
use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client};
use zerith_workflows::scheduler::{self, NoHooks, SchedulerState, TickOptions};
use zerith_workflows::{AiExecutor, Store, WorkflowError};

use crate::headless::{provider_config, resolve_model};

const NO_AI: &str = "conta de IA não configurada neste servidor";

/// Executor de IA do daemon: uma chamada de modelo com a conta das
/// configurações do servidor (`settings.toml`, env, cofre).
/// ponytail: o nó em modo "loop de agente" cai numa chamada simples (sem
/// ferramentas, MCP nem memória); trazer o `run_agent_turn` do desktop quando
/// algum workflow de servidor precisar de ferramentas.
pub struct DaemonAiExecutor {
    paths: Paths,
}

impl DaemonAiExecutor {
    fn settings(&self) -> Settings {
        Settings::load(&self.paths, None).settings
    }
}

impl AiExecutor for DaemonAiExecutor {
    fn complete(
        &self,
        model: Option<&str>,
        system: &str,
        prompt: &str,
    ) -> zerith_workflows::Result<String> {
        let settings = self.settings();
        let ai_err = |e: String| WorkflowError::Ai(format!("{NO_AI}: {e}"));
        let cfg = provider_config(&settings).map_err(|e| ai_err(e.to_string()))?;
        let model = resolve_model(model.map(str::to_string), &settings, &cfg)
            .ok_or_else(|| WorkflowError::Ai(format!("{NO_AI} (nenhum modelo definido)")))?;
        let client = Client::new(cfg).map_err(|e| ai_err(e.to_string()))?;
        let req = ChatRequest::new(
            &model,
            vec![ChatMessage::system(system), ChatMessage::user(prompt)],
        )
        .max_tokens(4096)
        .temperature(0.2);
        client
            .complete(&req, &Cancel::default())
            .map(|c| c.text)
            .map_err(|e| WorkflowError::Ai(zerith_providers::explain::headline(&e)))
    }

    fn resolve_bot(&self, bot_name: &str) -> Option<(Option<String>, String)> {
        zerith_core::agents::find(&self.paths, None, bot_name.trim())
            .filter(|a| !a.disabled)
            .map(|a| (a.model, a.instructions))
    }
}

/// O que o daemon mantém entre ciclos para os workflows.
pub struct WorkflowDaemon {
    root: std::path::PathBuf,
    store: Arc<Store>,
    ai: Arc<dyn AiExecutor>,
    state: SchedulerState,
}

impl WorkflowDaemon {
    pub fn open(paths: &Paths) -> zerith_workflows::Result<Self> {
        let root = paths.root().to_path_buf();
        Ok(Self {
            store: Arc::new(Store::open(&root.join("workflows.db"))?),
            ai: Arc::new(DaemonAiExecutor {
                paths: paths.clone(),
            }),
            state: SchedulerState::default(),
            root,
        })
    }

    /// Um ciclo: dispara os workflows vencidos e devolve as threads iniciadas.
    pub fn cycle(&self, now_secs: i64) -> Vec<JoinHandle<()>> {
        let opts = TickOptions {
            data_root: self.root.clone(),
            now_secs,
            tz_offset_minutes: None,
            hooks: Arc::new(NoHooks),
        };
        match scheduler::tick(&self.store, &self.ai, &opts, &self.state) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("zerith: erro ao verificar workflows: {e}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerith_workflows::{NodeKind, Workflow, WorkflowEdge, WorkflowNode};

    fn node(id: &str, kind: NodeKind, config: serde_json::Value) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            kind,
            label: id.into(),
            x: 0.0,
            y: 0.0,
            config,
        }
    }

    fn every_minute_wf(id: &str, second: WorkflowNode) -> Workflow {
        Workflow {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            edges: vec![WorkflowEdge {
                id: "e".into(),
                from: "c".into(),
                to: second.id.clone(),
                branch: None,
            }],
            nodes: vec![
                node(
                    "c",
                    NodeKind::TriggerCron,
                    json!({"cron": "* * * * *", "tz_offset_minutes": 0}),
                ),
                second,
            ],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        }
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zerith-dwf-{tag}-{}-{}",
            std::process::id(),
            zerith_workflows::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn daemon_cycle_runs_due_workflow_from_workflows_db() {
        let dir = temp_root("cycle");
        let paths = Paths::with_root(&dir);
        let st = Store::open(&dir.join("workflows.db")).unwrap();
        let wf = every_minute_wf(
            "wf-vps",
            node("t", NodeKind::Transform, json!({"template": "ok"})),
        );
        st.save_workflow(&wf).unwrap();

        let d = WorkflowDaemon::open(&paths).unwrap();
        let t = 1_700_000_000;
        d.cycle(t).into_iter().for_each(|h| h.join().unwrap());
        assert!(d.cycle(t + 10).is_empty(), "mesmo minuto não repete");
        let runs = st.list_runs("wf-vps", 5).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
