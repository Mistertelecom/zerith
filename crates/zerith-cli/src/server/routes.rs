//! Rotas JSON `/v1/...`. Nenhuma devolve valor de segredo nem o token.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::{json, Map, Value};
use zerith_core::secrets::{SecretError, SecretStore};
use zerith_workflows::remote::{Health, PutWorkflow, RunStarted};
use zerith_workflows::{
    execute_workflow, local_offset_minutes, now_ms, unix_secs, NodeEvent, RunOptions, Workflow,
    WorkflowRun,
};

use super::{auth, events, http::Request, Ctx};

/// Execuções simultâneas disparadas pela API.
const MAX_RUNS: usize = 8;

type Resp = (u16, Value);

fn not_found(what: &str) -> Resp {
    (404, json!({ "error": format!("{what} não encontrado") }))
}

fn internal(e: impl std::fmt::Display) -> Resp {
    (500, json!({ "error": e.to_string() }))
}

fn body_json<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, Resp> {
    serde_json::from_slice(&req.body)
        .map_err(|e| (400, json!({ "error": format!("JSON inválido: {e}") })))
}

fn ok<T: serde::Serialize>(v: &T) -> Resp {
    (200, serde_json::to_value(v).unwrap_or(Value::Null))
}

pub fn route(ctx: &Arc<Ctx>, req: &Request) -> Resp {
    let Some(rest) = req.path.strip_prefix("/v1/") else {
        return (404, json!({ "error": "rota não encontrada" }));
    };
    let segs: Vec<&str> = rest.split('/').collect();
    let res = match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["ping"]) => Ok((
            200,
            json!({ "name": "zerith", "version": zerith_core::VERSION }),
        )),
        ("GET", ["health"]) => health(ctx),
        ("GET", ["workflows"]) => ctx.store.list_workflows().map(|l| ok(&l)).map_err(internal),
        ("GET", ["workflows", id]) => get_wf(ctx, id).map(|w| ok(&w)),
        ("PUT", ["workflows", id]) => put_wf(ctx, id, req),
        ("DELETE", ["workflows", id]) => delete_wf(ctx, id),
        ("POST", ["workflows", id, "run"]) => run_wf(ctx, id, req),
        ("POST", ["workflows", id, "enabled"]) => set_enabled(ctx, id, req),
        ("GET", ["workflows", id, "runs"]) => {
            let limit = req
                .query_param("limit")
                .and_then(|l| l.parse::<usize>().ok())
                .unwrap_or(20)
                .clamp(1, 200);
            get_wf(ctx, id).and_then(|_| {
                ctx.store
                    .list_runs(id, limit)
                    .map(|l| ok(&l))
                    .map_err(internal)
            })
        }
        ("GET", ["runs", id]) => match get_run(&ctx.root, id) {
            Ok(Some(r)) => Ok(ok(&r)),
            Ok(None) => Err(not_found("run")),
            Err(e) => Err(internal(e)),
        },
        ("GET", ["secrets"]) => Ok((200, json!(secret_names(&ctx.root)))),
        ("PUT", ["secrets", name]) => put_secret(ctx, name, req),
        ("DELETE", ["secrets", name]) => delete_secret(ctx, name),
        _ => Err((404, json!({ "error": "rota não encontrada" }))),
    };
    res.unwrap_or_else(|e| e)
}

// ------------------------------------------------------------ health

pub fn health_value(ctx: &Ctx) -> Result<Health, Resp> {
    let wfs = ctx.store.list_workflows().map_err(internal)?;
    Ok(Health {
        name: "zerith".into(),
        version: zerith_core::VERSION.into(),
        hostname: super::hostname(),
        uptime_secs: ctx.started.elapsed().as_secs(),
        daemon: daemon_status(&ctx.root),
        workflows_total: wfs.len(),
        workflows_enabled: wfs.iter().filter(|w| w.enabled).count(),
        failing_workflows: wfs
            .iter()
            .filter(|w| w.last_status.as_deref() == Some("error"))
            .count(),
        runs_active: ctx.runs_active.load(Ordering::SeqCst),
    })
}

fn health(ctx: &Ctx) -> Result<Resp, Resp> {
    health_value(ctx).map(|h| ok(&h))
}

/// Última linha da tabela `daemon_status` em `<raiz>/workflows.db`, com as
/// colunas que existirem, como objeto JSON; `None` se a tabela não existe.
///
/// Contrato com o agendador no `zerith daemon` (feito em paralelo): ele grava
/// uma linha por batida nessa tabela (ex.: `pid`, `started_at_ms`,
/// `heartbeat_at_ms`, `last_error`). TODO(ligação): quando existir
/// `zerith_workflows::Store::daemon_status()`, trocar esta leitura por ela.
pub fn daemon_status(root: &Path) -> Option<Value> {
    use rusqlite::types::Value as Sql;
    let conn = rusqlite::Connection::open_with_flags(
        root.join("workflows.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let mut stmt = conn
        .prepare("SELECT * FROM daemon_status ORDER BY rowid DESC LIMIT 1")
        .ok()?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    stmt.query_row([], |r| {
        let mut m = Map::new();
        for (i, n) in names.iter().enumerate() {
            let v = match r.get::<_, Sql>(i)? {
                Sql::Integer(x) => json!(x),
                Sql::Real(x) => json!(x),
                Sql::Text(x) => json!(x),
                Sql::Null | Sql::Blob(_) => Value::Null,
            };
            m.insert(n.clone(), v);
        }
        Ok(Value::Object(m))
    })
    .ok()
}

// ------------------------------------------------------------ workflows

fn get_wf(ctx: &Ctx, id: &str) -> Result<Workflow, Resp> {
    ctx.store
        .get_workflow(id)
        .map_err(internal)?
        .ok_or_else(|| not_found("workflow"))
}

/// Concorrência otimista: o cliente manda o `updated_at_ms` que viu
/// (`base_updated_at_ms`). Se o servidor tem outro, 409 com a versão atual.
/// Cada gravação avança `updated_at_ms`, então duas edições no mesmo ms não
/// ficam com a mesma versão.
fn put_wf(ctx: &Ctx, id: &str, req: &Request) -> Result<Resp, Resp> {
    let put: PutWorkflow = body_json(req)?;
    let mut wf = put.workflow;
    if !wf.id.is_empty() && wf.id != id {
        return Err((400, json!({ "error": "id do corpo difere do id da URL" })));
    }
    wf.id = id.to_string();
    let _g = ctx.write_lock.lock().map_err(internal)?;
    let current = ctx.store.get_workflow(id).map_err(internal)?;
    let now = now_ms();
    match &current {
        Some(cur) if put.base_updated_at_ms != Some(cur.updated_at_ms) => {
            return Err((
                409,
                json!({ "error": "o servidor tem uma versão diferente", "current": cur }),
            ));
        }
        Some(cur) => {
            wf.created_at_ms = cur.created_at_ms;
            wf.updated_at_ms = now.max(cur.updated_at_ms + 1);
        }
        None => {
            wf.created_at_ms = now;
            wf.updated_at_ms = now;
        }
    }
    let saved = ctx.store.save_workflow(&wf).map_err(internal)?;
    Ok((if current.is_some() { 200 } else { 201 }, json!(saved)))
}

fn delete_wf(ctx: &Ctx, id: &str) -> Result<Resp, Resp> {
    get_wf(ctx, id)?;
    ctx.store.delete_workflow(id).map_err(internal)?;
    Ok((200, json!({ "deleted": id })))
}

fn set_enabled(ctx: &Ctx, id: &str, req: &Request) -> Result<Resp, Resp> {
    #[derive(serde::Deserialize)]
    struct Body {
        enabled: bool,
    }
    let b: Body = body_json(req)?;
    get_wf(ctx, id)?;
    ctx.store
        .set_workflow_enabled(id, b.enabled)
        .map_err(internal)?;
    get_wf(ctx, id).map(|w| ok(&w))
}

/// Libera a vaga de execução mesmo se a thread entrar em pânico.
struct RunSlot(Arc<Ctx>);
impl Drop for RunSlot {
    fn drop(&mut self) {
        self.0.runs_active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Dispara em thread e responde assim que o runner anuncia o run_id (o
/// primeiro evento de nó). Corpo opcional `{"input": "..."}`.
fn run_wf(ctx: &Arc<Ctx>, id: &str, req: &Request) -> Result<Resp, Resp> {
    #[derive(serde::Deserialize, Default)]
    struct Body {
        input: Option<String>,
    }
    let body: Body = if req.body.is_empty() {
        Body::default()
    } else {
        body_json(req)?
    };
    let wf = get_wf(ctx, id)?;
    if ctx.runs_active.fetch_add(1, Ordering::SeqCst) >= MAX_RUNS {
        ctx.runs_active.fetch_sub(1, Ordering::SeqCst);
        return Err((503, json!({ "error": "execuções demais em andamento" })));
    }
    let slot = RunSlot(Arc::clone(ctx));
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let c = Arc::clone(&slot.0);
        let tx_ev = tx.clone();
        let mut announced = false;
        let bus = &c.bus;
        let mut on_event = |ev: NodeEvent| {
            if !announced {
                announced = true;
                let _ = tx_ev.send(Ok(ev.run_id.clone()));
                bus.publish(
                    "run_started",
                    json!({ "run_id": ev.run_id, "workflow_id": ev.workflow_id }),
                );
            }
            bus.publish("node", json!(ev));
        };
        let mut opts = RunOptions {
            store: &c.store,
            data_root: &c.root,
            ai: c.ai.as_ref(),
            trigger_source: "remote",
            tz_offset_minutes: local_offset_minutes(unix_secs()),
            initial_input: body.input.as_deref(),
            on_event: &mut on_event,
        };
        match execute_workflow(&wf, &mut opts) {
            Ok(run) => {
                let _ = tx.send(Ok(run.id.clone()));
                c.bus.publish("run_finished", events::run_event(&run));
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                c.bus.publish(
                    "run_finished",
                    json!({ "workflow_id": wf.id, "status": "error", "summary": e.to_string() }),
                );
            }
        }
        drop(slot);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(run_id)) => Ok((202, json!(RunStarted { run_id }))),
        Ok(Err(e)) => Err((422, json!({ "error": e }))),
        Err(_) => Err((500, json!({ "error": "execução não começou em 10 s" }))),
    }
}

/// `Store` não tem busca de run por id (e o arquivo é de outro agente), então
/// lê direto da tabela `workflow_runs`, com as mesmas colunas de `list_runs`.
pub fn get_run(root: &Path, run_id: &str) -> rusqlite::Result<Option<WorkflowRun>> {
    use rusqlite::OptionalExtension;
    let conn = rusqlite::Connection::open_with_flags(
        root.join("workflows.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.query_row(
        "SELECT id, workflow_id, trigger_source, status, started_at_ms, finished_at_ms, summary, node_logs_json
         FROM workflow_runs WHERE id = ?1",
        [run_id],
        |r| {
            let logs: String = r.get(7)?;
            Ok(WorkflowRun {
                id: r.get(0)?,
                workflow_id: r.get(1)?,
                trigger_source: r.get(2)?,
                status: r.get(3)?,
                started_at_ms: r.get(4)?,
                finished_at_ms: r.get(5)?,
                summary: r.get(6)?,
                node_logs: serde_json::from_str(&logs).unwrap_or_default(),
            })
        },
    )
    .optional()
}

// ------------------------------------------------------------ segredos

/// O cofre do SO não lista entradas, então os nomes gravados por esta API
/// ficam num índice (só nomes) em `<raiz>/server/secrets.json`.
fn secrets_index(root: &Path) -> std::path::PathBuf {
    auth::server_dir(root).join("secrets.json")
}

pub fn secret_names(root: &Path) -> Vec<String> {
    std::fs::read(secrets_index(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_names(root: &Path, names: &[String]) -> Result<(), Resp> {
    let bytes = serde_json::to_vec(names).map_err(internal)?;
    auth::write_private(&secrets_index(root), &bytes).map_err(internal)
}

fn valid_secret_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn secret_err(e: SecretError) -> Resp {
    // A mensagem do cofre tem só o nome, nunca o valor.
    (503, json!({ "error": e.to_string() }))
}

fn put_secret(ctx: &Ctx, name: &str, req: &Request) -> Result<Resp, Resp> {
    #[derive(serde::Deserialize)]
    struct Body {
        value: String,
    }
    if !valid_secret_name(name) {
        return Err((
            400,
            json!({ "error": "nome: letras, números, '-', '_' e '.' (até 128)" }),
        ));
    }
    let b: Body = body_json(req)?;
    if b.value.is_empty() {
        return Err((400, json!({ "error": "valor vazio" })));
    }
    let _g = ctx.write_lock.lock().map_err(internal)?;
    ctx.secrets.set(name, &b.value).map_err(secret_err)?;
    let mut names = secret_names(&ctx.root);
    if !names.iter().any(|n| n == name) {
        names.push(name.to_string());
        names.sort();
        save_names(&ctx.root, &names)?;
    }
    Ok((200, json!({ "name": name, "stored": true })))
}

fn delete_secret(ctx: &Ctx, name: &str) -> Result<Resp, Resp> {
    if !valid_secret_name(name) {
        return Err((400, json!({ "error": "nome inválido" })));
    }
    let _g = ctx.write_lock.lock().map_err(internal)?;
    match ctx.secrets.delete(name) {
        Ok(()) | Err(SecretError::NotFound(_)) => {}
        Err(e) => return Err(secret_err(e)),
    }
    let mut names = secret_names(&ctx.root);
    names.retain(|n| n != name);
    save_names(&ctx.root, &names)?;
    Ok((200, json!({ "deleted": name })))
}

/// Cofre usado pelo servidor: o do SO em produção, um mapa nos testes.
pub type Secrets = Box<dyn SecretStore + Send + Sync>;
