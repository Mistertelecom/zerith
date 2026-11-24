//! Agendador de workflows reaproveitável pelo app desktop e pelo `zerith daemon`:
//! decide quais workflows vencem agora (cron + fuso, sem repetir no mesmo
//! minuto, recuperando minutos perdidos) e dispara cada um numa thread, sem
//! deixar o mesmo workflow rodar duas vezes ao mesmo tempo.
//!
//! Ideia (só conceito, código próprio): o loop de tick do agendador do Goose
//! (aaif-goose/goose `crates/goose/src/scheduler/`, Apache-2.0) e o
//! `cron/scheduler_tick.py` do Hermes (MIT).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::local_offset_minutes;
use crate::model::{NodeEvent, NodeKind, Workflow, WorkflowNode, WorkflowRun};
use crate::runner::{execute_workflow, AiExecutor, RunOptions};
use crate::schedule::CronSchedule;
use crate::store::Store;

/// Minutos perdidos (atraso do tick, máquina suspensa) que ainda disparam.
pub const MAX_CATCHUP_MINUTES: i64 = 5;

/// Memória do agendador entre ticks: último minuto UTC disparado por workflow
/// e último minuto verificado (para recuperar minutos pulados).
#[derive(Debug, Default, Clone)]
pub struct FireLog {
    last_fired: HashMap<String, i64>,
    last_checked_utc_min: Option<i64>,
}

/// Workflow vencido: `utc_min` é o minuto (UNIX/60) que bateu e
/// `tz_offset_minutes` o fuso que a execução deve usar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueWorkflow {
    pub id: String,
    pub utc_min: i64,
    pub tz_offset_minutes: i32,
}

/// Fuso explícito gravado no gatilho cron, se houver.
fn node_tz(node: &WorkflowNode) -> Option<i32> {
    node.config
        .get("tz_offset_minutes")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
}

/// Workflows ativos com gatilho cron batendo entre o último minuto verificado
/// e `now_utc_secs` (no máximo [`MAX_CATCHUP_MINUTES`] para trás). Cada um sai
/// no máximo uma vez por chamada e nunca duas vezes no mesmo minuto UTC.
///
/// Fuso de cada gatilho: o `tz_offset_minutes` do nó; senão `tz_offset_min`
/// (padrão do servidor); senão o horário local do sistema naquele minuto
/// (com horário de verão).
pub fn due_workflows(
    workflows: &[Workflow],
    now_utc_secs: i64,
    tz_offset_min: Option<i32>,
    log: &mut FireLog,
) -> Vec<DueWorkflow> {
    let now_utc_min = now_utc_secs.div_euclid(60);
    let start_min = match log.last_checked_utc_min {
        Some(prev) if prev < now_utc_min => (prev + 1).max(now_utc_min - MAX_CATCHUP_MINUTES),
        _ => now_utc_min,
    };
    log.last_checked_utc_min = Some(now_utc_min);

    let mut due: Vec<DueWorkflow> = Vec::new();
    for utc_min in start_min..=now_utc_min {
        let check_secs = utc_min * 60;
        for wf in workflows {
            if !wf.enabled || due.iter().any(|d| d.id == wf.id) {
                continue;
            }
            for node in wf.nodes.iter().filter(|n| n.kind == NodeKind::TriggerCron) {
                let expr = node
                    .config
                    .get("cron")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0 8 * * *");
                let fixed = node_tz(node).or(tz_offset_min);
                let tz = fixed.unwrap_or_else(|| local_offset_minutes(check_secs));
                let Ok(sched) = CronSchedule::parse(expr) else {
                    continue;
                };
                if !sched.matches_unix(check_secs, tz) {
                    continue;
                }
                if log.last_fired.get(&wf.id) != Some(&utc_min) {
                    log.last_fired.insert(wf.id.clone(), utc_min);
                    due.push(DueWorkflow {
                        id: wf.id.clone(),
                        utc_min,
                        tz_offset_minutes: fixed
                            .unwrap_or_else(|| local_offset_minutes(now_utc_secs)),
                    });
                }
                break;
            }
        }
    }
    due
}

/// Estado do agendador de um processo: minutos já disparados e workflows em
/// execução agora.
#[derive(Default)]
pub struct SchedulerState {
    fires: Mutex<FireLog>,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

/// Trava de execução de um workflow; solta ao sair de escopo (inclusive em
/// pânico da thread).
pub struct InFlight {
    set: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl SchedulerState {
    /// `None` se o workflow já está rodando neste processo.
    pub fn try_begin_run(&self, id: &str) -> Option<InFlight> {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string())
            .then(|| InFlight {
                set: Arc::clone(&self.in_flight),
                id: id.to_string(),
            })
    }
}

/// Falhas seguidas mais recentes do workflow, contadas no histórico de
/// execuções (sobrevive a reinício do processo). Execução em andamento não
/// conta nem interrompe a sequência.
pub fn consecutive_failures(store: &Store, workflow_id: &str) -> crate::Result<u32> {
    let mut n = 0;
    for run in store.list_runs(workflow_id, 200)? {
        match run.status.as_str() {
            "running" => continue,
            "error" => n += 1,
            _ => break,
        }
    }
    Ok(n)
}

/// Ganchos de quem hospeda o agendador (eventos Tauri, chat do bot, log).
pub trait RunHooks: Send + Sync {
    fn on_event(&self, _ev: &NodeEvent) {}
    fn on_done(&self, _wf: &Workflow, _run: &WorkflowRun) {}
}

/// Ganchos vazios.
pub struct NoHooks;
impl RunHooks for NoHooks {}

pub struct TickOptions {
    /// Raiz de dados do Zerith (passada ao motor como `data_root`).
    pub data_root: PathBuf,
    pub now_secs: i64,
    /// Fuso padrão do servidor; `None` = horário local do sistema.
    pub tz_offset_minutes: Option<i32>,
    pub hooks: Arc<dyn RunHooks>,
}

/// Um ciclo do agendador: dispara cada workflow vencido numa thread própria
/// (gatilho `cron`) e devolve as threads iniciadas. Workflow ainda rodando de
/// um ciclo anterior é pulado em vez de sobrepor.
pub fn tick(
    store: &Arc<Store>,
    ai: &Arc<dyn AiExecutor>,
    opts: &TickOptions,
    state: &SchedulerState,
) -> crate::Result<Vec<JoinHandle<()>>> {
    let wfs = store.list_workflows()?;
    let due = {
        let mut log = state.fires.lock().unwrap_or_else(|e| e.into_inner());
        due_workflows(&wfs, opts.now_secs, opts.tz_offset_minutes, &mut log)
    };
    let mut handles = Vec::new();
    for d in due {
        let Some(wf) = wfs.iter().find(|w| w.id == d.id).cloned() else {
            continue;
        };
        let Some(guard) = state.try_begin_run(&wf.id) else {
            continue;
        };
        let (store, ai, hooks) = (Arc::clone(store), Arc::clone(ai), Arc::clone(&opts.hooks));
        let root = opts.data_root.clone();
        handles.push(std::thread::spawn(move || {
            let _guard = guard;
            let mut emit = |ev: NodeEvent| hooks.on_event(&ev);
            let mut ro = RunOptions {
                store: &store,
                data_root: &root,
                ai: ai.as_ref(),
                trigger_source: "cron",
                tz_offset_minutes: d.tz_offset_minutes,
                initial_input: None,
                on_event: &mut emit,
            };
            if let Ok(run) = execute_workflow(&wf, &mut ro) {
                hooks.on_done(&wf, &run);
            }
        }));
    }
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn cron_wf(id: &str, nodes: Vec<WorkflowNode>) -> Workflow {
        Workflow {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            icon: "workflow".into(),
            enabled: true,
            nodes,
            edges: vec![],
            created_at_ms: 0,
            updated_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            bot_name: None,
        }
    }

    pub(crate) fn cron_node(id: &str, cron: &str, tz: Option<i32>) -> WorkflowNode {
        let mut config = json!({ "cron": cron });
        if let Some(tz) = tz {
            config["tz_offset_minutes"] = json!(tz);
        }
        WorkflowNode {
            id: id.into(),
            kind: NodeKind::TriggerCron,
            label: id.into(),
            x: 0.0,
            y: 0.0,
            config,
        }
    }

    /// 2023-11-14 22:13:20 UTC.
    const T: i64 = 1_700_000_000;

    #[test]
    fn cron_hits_once_per_minute_even_with_two_triggers() {
        let wfs = vec![cron_wf(
            "wf",
            vec![
                cron_node("c1", "* * * * *", Some(-180)),
                cron_node("c2", "* * * * *", Some(0)),
            ],
        )];
        let mut log = FireLog::default();
        assert_eq!(due_workflows(&wfs, T, None, &mut log).len(), 1);
        assert!(due_workflows(&wfs, T + 30, None, &mut log).is_empty());
        assert_eq!(due_workflows(&wfs, T + 60, None, &mut log).len(), 1);
        // Desligado não dispara.
        let mut off = wfs.clone();
        off[0].enabled = false;
        assert!(due_workflows(&off, T + 120, None, &mut log).is_empty());
    }

    #[test]
    fn catch_up_recovers_skipped_minute_and_caps() {
        let at04 = (T / 3600) * 3600 + 4 * 60; // hh:04 UTC
        let wfs = vec![cron_wf("m5", vec![cron_node("c", "5 * * * *", Some(0))])];
        let mut log = FireLog::default();
        assert!(due_workflows(&wfs, at04 + 10, None, &mut log).is_empty());
        let r = due_workflows(&wfs, at04 + 130, None, &mut log);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].utc_min, at04 / 60 + 1);
        assert!(due_workflows(&wfs, at04 + 160, None, &mut log).is_empty());

        // Atraso grande: limita a 5 minutos e junta numa execução só.
        let every = vec![cron_wf("e", vec![cron_node("c", "* * * * *", Some(0))])];
        let mut log = FireLog {
            last_checked_utc_min: Some(1000),
            ..FireLog::default()
        };
        assert_eq!(
            due_workflows(&every, 1010 * 60 + 5, None, &mut log).len(),
            1
        );
    }

    #[test]
    fn timezone_node_beats_server_default_beats_local() {
        // 11:00 UTC = 08:00 em Brasília (-180).
        let at11 = T - T.rem_euclid(86_400) + 11 * 3600;
        let node_brt = vec![cron_wf("a", vec![cron_node("c", "0 8 * * *", Some(-180))])];
        let no_tz = vec![cron_wf("b", vec![cron_node("c", "0 8 * * *", None)])];

        let d = due_workflows(&node_brt, at11, Some(0), &mut FireLog::default());
        assert_eq!(d[0].tz_offset_minutes, -180);
        // Sem fuso no nó: o padrão do servidor decide.
        let d = due_workflows(&no_tz, at11, Some(-180), &mut FireLog::default());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].tz_offset_minutes, -180);
        assert!(due_workflows(&no_tz, at11, Some(0), &mut FireLog::default()).is_empty());
    }

    #[test]
    fn in_flight_blocks_concurrent_run_and_releases_on_drop() {
        let st = SchedulerState::default();
        let g = st.try_begin_run("wf").unwrap();
        assert!(st.try_begin_run("wf").is_none());
        assert!(st.try_begin_run("outro").is_some());
        drop(g);
        assert!(st.try_begin_run("wf").is_some());
    }

    #[test]
    fn tick_runs_due_workflow_and_skips_while_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("w.db")).unwrap());
        let mut wf = cron_wf("t", vec![cron_node("c", "* * * * *", Some(0))]);
        wf.nodes.push(WorkflowNode {
            id: "x".into(),
            kind: NodeKind::Transform,
            label: "x".into(),
            x: 0.0,
            y: 0.0,
            config: json!({ "template": "oi" }),
        });
        wf.edges.push(crate::WorkflowEdge {
            id: "e".into(),
            from: "c".into(),
            to: "x".into(),
            branch: None,
        });
        store.save_workflow(&wf).unwrap();
        let ai: Arc<dyn AiExecutor> = Arc::new(crate::NoopAiExecutor);
        let state = SchedulerState::default();
        let opts = |now| TickOptions {
            data_root: dir.path().to_path_buf(),
            now_secs: now,
            tz_offset_minutes: None,
            hooks: Arc::new(NoHooks),
        };

        // Rodando: o minuto seguinte não sobrepõe.
        let busy = state.try_begin_run("t").unwrap();
        assert!(tick(&store, &ai, &opts(T), &state).unwrap().is_empty());
        drop(busy);

        let hs = tick(&store, &ai, &opts(T + 60), &state).unwrap();
        assert_eq!(hs.len(), 1);
        hs.into_iter().for_each(|h| h.join().unwrap());
        let runs = store.list_runs("t", 5).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].trigger_source, "cron");
        assert_eq!(consecutive_failures(&store, "t").unwrap(), 0);
    }

    #[test]
    fn consecutive_failures_counts_latest_error_streak() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("w.db")).unwrap();
        for (i, st) in ["error", "ok", "error", "error"].iter().enumerate() {
            let id = format!("r{i}");
            store.record_run_start(&id, "w", "cron").unwrap();
            store.record_run_finish(&id, "w", st, "", &[]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert_eq!(consecutive_failures(&store, "w").unwrap(), 2);
        store.record_run_start("r9", "w", "cron").unwrap();
        assert_eq!(consecutive_failures(&store, "w").unwrap(), 2);
    }
}
