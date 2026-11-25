//! Ponta a ponta com o binário de verdade: `zerith server init` + `serve` numa
//! porta efêmera e `ZERITH_HOME` temporário, o desktop simulado pelo
//! `RemoteClient` (envia workflow com nó shell, dispara, confere status/log) e
//! um ciclo `zerith daemon --once` que roda um gatilho cron vencido e grava o
//! sinal de vida. Cofre em arquivo dentro da raiz temporária: nada vai para o
//! cofre real do sistema.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use zerith_workflows::remote::{PairingCode, RemoteClient};
use zerith_workflows::Workflow;

fn zerith(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_zerith"));
    c.current_dir(root)
        .env("ZERITH_HOME", root)
        .env("ZERITH_SECRETS_BACKEND", "file")
        .env_remove("ZERITH_API_KEY")
        .stdin(Stdio::null());
    c
}

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wf(id: &str, trigger: &str, enabled: bool) -> Workflow {
    serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "enabled": enabled, "created_at_ms": 0, "updated_at_ms": 0,
        "nodes": [
            {"id": "t", "kind": trigger, "label": "T", "x": 0, "y": 0,
             "config": {"cron": "* * * * *"}},
            {"id": "s", "kind": "shell", "label": "S", "x": 1, "y": 0,
             "config": {"command": "echo ok", "timeout_secs": 20}}
        ],
        "edges": [{"id": "e", "from": "t", "to": "s"}]
    }))
    .unwrap()
}

fn wait_run(c: &RemoteClient, run_id: &str) -> zerith_workflows::WorkflowRun {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run = c.run_get(run_id).unwrap();
        if run.status != "running" || Instant::now() > deadline {
            return run;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn servidor_real_executa_workflow_e_daemon_grava_sinal_de_vida() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let init = zerith(root)
        .args(["server", "init", "--name", "teste", "--bind"])
        .arg(format!("127.0.0.1:{port}"))
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let out = String::from_utf8_lossy(&init.stdout);
    let pairing = out
        .lines()
        .find_map(|l| PairingCode::decode(l.trim()).ok())
        .expect("código de pareamento na saída do init");
    assert_eq!(pairing.url, format!("http://127.0.0.1:{port}"));

    let _server = Kill(
        zerith(root)
            .args(["server", "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let c = RemoteClient::from_pairing(&pairing);
    let deadline = Instant::now() + Duration::from_secs(20);
    while c.ping().is_err() {
        assert!(Instant::now() < deadline, "servidor não respondeu");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Desktop: envia, dispara e acompanha.
    c.put(&wf("manual", "trigger_manual", false), None).unwrap();
    let run = wait_run(&c, &c.run("manual", None).unwrap());
    assert_eq!(run.status, "ok", "{run:?}");
    let shell = run.node_logs.iter().find(|l| l.node_id == "s").unwrap();
    assert_eq!(shell.output.trim(), "ok");
    assert_eq!(c.runs("manual", 5).unwrap()[0].id, run.id);
    assert!(
        c.health().unwrap().daemon.is_none(),
        "daemon ainda não rodou"
    );

    // Daemon: um ciclo com gatilho `* * * * *` (sempre vencido).
    c.put(&wf("agendado", "trigger_cron", true), None).unwrap();
    let once = zerith(root).args(["daemon", "--once"]).output().unwrap();
    assert!(once.status.success(), "{once:?}");
    let runs = c.runs("agendado", 5).unwrap();
    assert_eq!(runs.len(), 1, "um ciclo, uma execução: {runs:?}");
    assert_eq!(
        (runs[0].status.as_str(), runs[0].trigger_source.as_str()),
        ("ok", "cron")
    );
    let daemon = c.health().unwrap().daemon.expect("daemon_status gravado");
    assert!(
        daemon["last_cycle_ms"].as_i64().unwrap_or(0) > 0,
        "{daemon}"
    );
    assert_eq!(daemon["active_workflows"], 1);
}
