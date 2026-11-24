//! `zerith server`: API HTTP para o desktop gerenciar workflows num servidor
//! Linux (ou qualquer máquina) cadastrado. Veja `docs/servidor.md`.
//!
//! Sem dependência nova: `std::net::TcpListener`, uma thread por conexão,
//! parser HTTP/1.1 próprio ([`http`]). Sem TLS próprio — o transporte cifrado
//! fica com WireGuard/Tailscale ou um proxy HTTPS (Caddy).
//!
//! Ideias (só referência, nada copiado): servidor do opencode
//! (anomalyco/opencode, MIT, `packages/opencode/src/server/`, v1.19.6) e o
//! pareamento por código do Hermes (MIT, `gateway/pairing.py`).

mod auth;
mod events;
mod http;
mod routes;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_core::Paths;
use zerith_workflows::{AiExecutor, Store};

type Boxed = Box<dyn std::error::Error>;

pub const DEFAULT_BIND: &str = "127.0.0.1:7878";
/// Conexões simultâneas (cada uma é uma thread; SSE conta enquanto aberto).
const MAX_CONNS: usize = 64;

/// Estado compartilhado por todas as conexões.
pub(crate) struct Ctx {
    pub root: PathBuf,
    pub started: Instant,
    pub store: Store,
    pub ai: Arc<dyn AiExecutor>,
    pub secrets: routes::Secrets,
    pub bus: events::EventBus,
    pub runs_active: AtomicUsize,
    /// Serializa ler-comparar-gravar (PUT com versão, índice de segredos).
    pub write_lock: Mutex<()>,
    conns: AtomicUsize,
    limiter: auth::FailLimiter,
    usage_lock: Mutex<()>,
}

impl Ctx {
    /// `<raiz>/workflows.db` (o mesmo banco do desktop e do daemon) e o cofre
    /// do sistema. Não semeia os modelos de exemplo: o servidor é gerenciado
    /// pelo desktop.
    pub(crate) fn open(paths: &Paths) -> Result<Self, Boxed> {
        Self::with(
            paths,
            Box::new(zerith_core::secrets::OsKeyring),
            Arc::new(CliAi {
                paths: paths.clone(),
            }),
        )
    }

    pub(crate) fn with(
        paths: &Paths,
        secrets: routes::Secrets,
        ai: Arc<dyn AiExecutor>,
    ) -> Result<Self, Boxed> {
        let root = paths.root().to_path_buf();
        Ok(Self {
            store: Store::open(&root.join("workflows.db"))?,
            root,
            started: Instant::now(),
            ai,
            secrets,
            bus: events::EventBus::default(),
            runs_active: AtomicUsize::new(0),
            write_lock: Mutex::new(()),
            conns: AtomicUsize::new(0),
            limiter: auth::FailLimiter::default(),
            usage_lock: Mutex::new(()),
        })
    }
}

/// Nós `ai_agent` no servidor: uma chamada ao provedor configurado em
/// `<raiz>` (mesma resolução de conta/modelo do `zerith -p`). Sem loop de
/// ferramentas — `complete_agent_loop` cai no padrão do trait.
// ponytail: só `complete`; ligar o loop do agente com ferramentas quando o
// daemon tiver um executor próprio (aí reaproveitar o dele).
struct CliAi {
    paths: Paths,
}

impl AiExecutor for CliAi {
    fn complete(
        &self,
        model: Option<&str>,
        system: &str,
        prompt: &str,
    ) -> zerith_workflows::Result<String> {
        use zerith_providers::{Cancel, ChatMessage, ChatRequest, Client};
        let err = |e: String| zerith_workflows::WorkflowError::Ai(e);
        let settings = crate::headless::load_settings(&self.paths);
        let cfg = crate::headless::provider_config(&settings).map_err(|e| err(e.to_string()))?;
        let model = crate::headless::resolve_model(model.map(str::to_string), &settings, &cfg)
            .ok_or_else(|| err("nenhum modelo configurado no servidor".into()))?;
        let client = Client::new(cfg).map_err(|e| err(e.to_string()))?;
        let req = ChatRequest::new(
            &model,
            vec![ChatMessage::system(system), ChatMessage::user(prompt)],
        )
        .max_tokens(4096);
        client
            .complete(&req, &Cancel::default())
            .map(|c| c.text)
            .map_err(|e| err(zerith_providers::explain::headline(&e)))
    }
}

/// Aceita conexões em `listener` numa thread própria até o processo sair.
pub(crate) fn run_on(listener: TcpListener, ctx: Arc<Ctx>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            let ctx = Arc::clone(&ctx);
            if ctx.conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS {
                ctx.conns.fetch_sub(1, Ordering::SeqCst);
                let mut s = stream;
                let _ = http::write_json(&mut s, 503, r#"{"error":"servidor ocupado"}"#);
                continue;
            }
            std::thread::spawn(move || {
                handle_conn(&ctx, stream);
                ctx.conns.fetch_sub(1, Ordering::SeqCst);
            });
        }
    })
}

fn handle_conn(ctx: &Arc<Ctx>, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let peer = stream.peer_addr().ok();
    let req = match http::read_request(&mut stream) {
        Ok(r) => r,
        Err(Some(e)) => {
            let _ = http::write_json(
                &mut stream,
                e.status,
                &json!({ "error": e.msg }).to_string(),
            );
            finish(stream);
            return;
        }
        Err(None) => return,
    };
    let (status, body) = match authorize(ctx, &req, peer) {
        Ok(()) if req.method == "GET" && req.path == "/v1/events" => {
            let since = req
                .query_param("since")
                .or(req.header("last-event-id"))
                .and_then(|s| s.parse().ok());
            let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
            let status =
                || serde_json::to_value(routes::health_value(ctx).ok()).unwrap_or_default();
            events::serve_sse(&mut stream, &ctx.bus, since, &status);
            return;
        }
        Ok(()) => routes::route(ctx, &req),
        Err(denied) => denied,
    };
    let _ = http::write_json(&mut stream, status, &body.to_string());
    finish(stream);
}

/// Fecha a escrita e drena um pouco do que sobrou (tempo e tamanho limitados)
/// para o Windows não mandar RST por cima da resposta em erros precoces.
fn finish(mut stream: TcpStream) {
    use std::io::Read;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut buf = [0u8; 4096];
    let mut total = 0;
    while total < 65_536 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

/// Bearer obrigatório em tudo menos `GET /v1/ping`. Falhas contam por IP;
/// passou do limite, 429 até a janela de 1 minuto esvaziar. O token nunca
/// aparece em resposta nem em log.
fn authorize(ctx: &Ctx, req: &http::Request, peer: Option<SocketAddr>) -> Result<(), (u16, Value)> {
    if req.method == "GET" && req.path == "/v1/ping" {
        return Ok(());
    }
    let ip = peer.map_or(std::net::IpAddr::from([0, 0, 0, 0]), |p| p.ip());
    if ctx.limiter.blocked(ip) {
        return Err((
            429,
            json!({ "error": "muitas falhas de autenticação; tente em 1 minuto" }),
        ));
    }
    match req.bearer().and_then(|t| auth::authenticate(&ctx.root, t)) {
        Some(device) => {
            auth::touch_usage(&ctx.root, &device, &ctx.usage_lock);
            Ok(())
        }
        None => {
            ctx.limiter.fail(ip);
            Err((401, json!({ "error": "token ausente ou inválido" })))
        }
    }
}

/// Liga o servidor configurado em `<raiz>/server/` e devolve a thread que
/// aceita conexões. Pensado para o `zerith daemon` chamar
/// (`let _h = server::spawn(&paths)?;`) e seguir com o agendador; `zerith
/// server serve` chama e espera a thread.
///
/// Falha se não houver `config.toml` (rode `zerith server init`), se o
/// endereço for público sem `allow_public = true`, ou se a porta estiver em uso.
pub fn spawn(paths: &Paths) -> Result<JoinHandle<()>, Boxed> {
    let cfg = auth::load_config(paths.root())?;
    let addr = auth::check_bind(&cfg.bind, cfg.allow_public)?;
    let listener = TcpListener::bind(addr)?;
    if auth::classify(addr.ip()) == auth::BindKind::Public {
        eprintln!(
            "
{}
",
            auth::PUBLIC_WARNING
        );
    }
    eprintln!(
        "zerith server '{}' ouvindo em http://{} ({} dispositivo(s) pareado(s))",
        cfg.name,
        listener.local_addr()?,
        auth::load_tokens(paths.root()).len()
    );
    Ok(run_on(listener, Arc::new(Ctx::open(paths)?)))
}

/// `zerith server ...`
pub fn cmd(paths: &Paths, args: &[String]) -> Result<(), Boxed> {
    let root = paths.root();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    match (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) {
        (Some("init"), _) => {
            if auth::config_path(root).exists() {
                return Err(format!(
                    "{} já existe; para outro dispositivo use `zerith server token add <nome>`",
                    auth::config_path(root).display()
                )
                .into());
            }
            let allow_public = args.iter().any(|a| a == "--allow-public");
            let bind = flag("--bind").unwrap_or_else(|| DEFAULT_BIND.into());
            let addr = auth::check_bind(&bind, allow_public)?;
            let url =
                match flag("--url") {
                    Some(u) => u,
                    None if addr.ip().is_unspecified() => return Err(
                        "com bind em todas as interfaces, informe --url <http://ip-ou-nome:porta> \
                                (o endereço que o desktop vai usar)"
                            .into(),
                    ),
                    None => format!("http://{addr}"),
                };
            let cfg = auth::ServerConfig {
                name: flag("--name").unwrap_or_else(hostname),
                bind: addr.to_string(),
                url,
                allow_public,
            };
            auth::save_config(root, &cfg)?;
            let device = flag("--device").unwrap_or_else(|| "desktop".into());
            let token = auth::add_token(root, &device)?;
            println!(
                "servidor '{}' configurado em {}",
                cfg.name,
                auth::config_path(root).display()
            );
            if auth::classify(addr.ip()) == auth::BindKind::Public {
                println!("\n{}", auth::PUBLIC_WARNING);
            }
            print_pairing(&cfg, &device, token);
            Ok(())
        }
        (Some("token"), Some("add")) => {
            let device = args
                .get(3)
                .ok_or("uso: zerith server token add <dispositivo>")?;
            let cfg = auth::load_config(root)?;
            let token = auth::add_token(root, device)?;
            print_pairing(&cfg, device, token);
            Ok(())
        }
        (Some("token"), Some("list")) => {
            let usage = auth::load_usage(root);
            let list = auth::load_tokens(root);
            if list.is_empty() {
                println!("(nenhum dispositivo pareado)");
            }
            for t in list {
                let last = usage
                    .get(&t.device)
                    .map_or("nunca".into(), |&ms| fmt_ms(ms));
                println!(
                    "{:<24} criado {}  último uso {}",
                    t.device,
                    fmt_ms(t.created_at_ms),
                    last
                );
            }
            Ok(())
        }
        (Some("token"), Some("revoke")) => {
            let device = args
                .get(3)
                .ok_or("uso: zerith server token revoke <dispositivo>")?;
            auth::revoke_token(root, device)?;
            println!("token de '{device}' revogado (vale na hora, sem reiniciar o servidor)");
            Ok(())
        }
        (Some("serve"), _) => {
            let h = spawn(paths)?;
            h.join()
                .map_err(|_| "thread do servidor terminou com pânico")?;
            Ok(())
        }
        _ => Err(
            "uso: zerith server init [--name <nome>] [--bind <ip:porta>] [--url <url>] \
                  [--device <nome>] [--allow-public]\n\
                  \x20     zerith server token add <dispositivo> | list | revoke <dispositivo>\n\
                  \x20     zerith server serve"
                .into(),
        ),
    }
}

/// Imprime o código (sozinho na linha, fácil de copiar) e, separado, como usar.
/// O token só existe aqui e no código; o servidor guarda apenas o hash.
fn print_pairing(cfg: &auth::ServerConfig, device: &str, token: String) {
    let code = zerith_workflows::remote::PairingCode {
        v: 1,
        name: cfg.name.clone(),
        url: cfg.url.clone(),
        token,
    }
    .encode();
    println!("\ncódigo de pareamento para '{device}' (trate como senha; não aparece de novo):\n");
    println!("{code}\n");
    println!(
        "como usar:\n\
         \x20 1. no Zerith desktop, abra Workflows > Servidores > Adicionar servidor;\n\
         \x20 2. cole o código acima e confirme;\n\
         \x20 3. o desktop vai chamar {}/v1/ping e depois /v1/health com o token.\n\
         o desktop precisa alcançar {} (Tailscale/WireGuard ou proxy HTTPS; veja docs/servidor.md).\n\
         revogar: zerith server token revoke {device}",
        cfg.url, cfg.url
    );
}

/// Nome da máquina: `/etc/hostname`, `HOSTNAME`, `COMPUTERNAME`, senão "zerith".
pub(crate) fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zerith".into())
}

/// `AAAA-MM-DD HH:MM` no fuso local.
fn fmt_ms(ms: i64) -> String {
    let secs = ms / 1000;
    let dt = zerith_workflows::LocalDateTime::from_unix_secs(
        secs,
        zerith_workflows::local_offset_minutes(secs),
    );
    format!("{} {}", dt.date_string(), &dt.time_string()[..5])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Cofre de teste em memória: nada vai para o Gerenciador de Credenciais.
    #[derive(Default, Clone)]
    pub(crate) struct MapSecrets(Arc<Mutex<std::collections::HashMap<String, String>>>);

    impl zerith_core::secrets::SecretStore for MapSecrets {
        fn get(&self, name: &str) -> Result<String, zerith_core::secrets::SecretError> {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| zerith_core::secrets::SecretError::NotFound(name.into()))
        }
        fn set(&self, name: &str, value: &str) -> Result<(), zerith_core::secrets::SecretError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<(), zerith_core::secrets::SecretError> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
    }

    /// Sobe o servidor numa porta efêmera de 127.0.0.1 sobre uma raiz
    /// temporária, com cofre em memória e IA de mentira.
    pub(crate) fn start_with(root: &std::path::Path, secrets: MapSecrets) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ctx = Ctx::with(
            &Paths::with_root(root),
            Box::new(secrets),
            Arc::new(zerith_workflows::NoopAiExecutor),
        )
        .unwrap();
        run_on(listener, Arc::new(ctx));
        addr
    }

    pub(crate) fn start(root: &std::path::Path) -> SocketAddr {
        start_with(root, MapSecrets::default())
    }

    /// Requisição crua; devolve (status, corpo JSON).
    pub(crate) fn call(
        addr: SocketAddr,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> (u16, Value) {
        let mut s = TcpStream::connect(addr).unwrap();
        let body = body.map(Value::to_string).unwrap_or_default();
        let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let status = out[9..12].parse().unwrap();
        let json = out
            .split_once("\r\n\r\n")
            .and_then(|(_, b)| serde_json::from_str(b).ok())
            .unwrap_or(Value::Null);
        (status, json)
    }

    #[test]
    fn ping_livre_e_resto_exige_token_valido() {
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let (st, v) = call(addr, "GET", "/v1/ping", None, None);
        assert_eq!((st, v["name"].as_str()), (200, Some("zerith")));
        assert_eq!(call(addr, "GET", "/v1/health", None, None).0, 401);
        assert_eq!(call(addr, "GET", "/v1/health", Some("zt_x"), None).0, 401);

        let tok = auth::add_token(dir.path(), "pc").unwrap();
        assert_ne!(call(addr, "GET", "/v1/health", Some(&tok), None).0, 401);
        assert!(
            auth::load_usage(dir.path()).contains_key("pc"),
            "último uso gravado"
        );
        auth::revoke_token(dir.path(), "pc").unwrap();
        let (st, v) = call(addr, "GET", "/v1/health", Some(&tok), None);
        assert_eq!(st, 401, "revogado vale sem reiniciar");
        assert!(!v.to_string().contains(&tok), "resposta não ecoa o token");
    }

    #[test]
    fn falhas_demais_viram_429() {
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        for _ in 0..auth::MAX_AUTH_FAILURES {
            assert_eq!(
                call(addr, "GET", "/v1/workflows", Some("zt_x"), None).0,
                401
            );
        }
        assert_eq!(
            call(addr, "GET", "/v1/workflows", Some("zt_x"), None).0,
            429
        );
        // Bloqueado por IP: nem o token certo passa até a janela esvaziar.
        assert_eq!(call(addr, "GET", "/v1/workflows", Some(&tok), None).0, 429);
        assert_eq!(call(addr, "GET", "/v1/ping", None, None).0, 200);
    }

    #[test]
    fn init_cria_config_e_token_e_recusa_bind_publico() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        let a = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(cmd(&paths, &a("server init --bind 0.0.0.0:7878")).is_err());
        assert!(!auth::config_path(dir.path()).exists());
        cmd(
            &paths,
            &a("server init --name casa --bind 198.18.251.1:7979"),
        )
        .unwrap();
        let cfg = auth::load_config(dir.path()).unwrap();
        assert_eq!(
            (cfg.name.as_str(), cfg.url.as_str()),
            ("casa", "http://198.18.251.1:7979")
        );
        assert_eq!(auth::load_tokens(dir.path())[0].device, "desktop");
        assert!(cmd(&paths, &a("server init")).is_err(), "não sobrescreve");
        cmd(&paths, &a("server token add celular")).unwrap();
        cmd(&paths, &a("server token revoke desktop")).unwrap();
        let left: Vec<_> = auth::load_tokens(dir.path())
            .into_iter()
            .map(|t| t.device)
            .collect();
        assert_eq!(left, ["celular"]);
    }

    fn sample_wf(id: &str) -> Value {
        json!({
            "id": id, "name": "teste", "created_at_ms": 0, "updated_at_ms": 0,
            "nodes": [
                {"id": "t", "kind": "trigger_manual", "label": "T", "x": 0, "y": 0},
                {"id": "a", "kind": "transform", "label": "A", "x": 1, "y": 0,
                 "config": {"mode": "template", "template": "oi {{prev}}"}}
            ],
            "edges": [{"id": "e", "from": "t", "to": "a"}]
        })
    }

    #[test]
    fn rotas_de_workflow_concorrencia_e_runs() {
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        let t = Some(tok.as_str());

        let (st, list) = call(addr, "GET", "/v1/workflows", t, None);
        assert_eq!(
            (st, list.as_array().map(Vec::len)),
            (200, Some(0)),
            "sem seed"
        );
        let put = json!({ "workflow": sample_wf("w1") });
        let (st, saved) = call(addr, "PUT", "/v1/workflows/w1", t, Some(&put));
        assert_eq!(st, 201);
        let v1 = saved["updated_at_ms"].as_i64().unwrap();
        // Recriar sem versão = conflito (já existe).
        assert_eq!(call(addr, "PUT", "/v1/workflows/w1", t, Some(&put)).0, 409);
        let upd = json!({ "workflow": sample_wf("w1"), "base_updated_at_ms": v1 });
        let (st, saved) = call(addr, "PUT", "/v1/workflows/w1", t, Some(&upd));
        assert_eq!(st, 200);
        assert!(saved["updated_at_ms"].as_i64().unwrap() > v1);
        // Mesma base de novo: o servidor já tem versão mais nova.
        let (st, v) = call(addr, "PUT", "/v1/workflows/w1", t, Some(&upd));
        assert_eq!(st, 409);
        assert_eq!(v["current"]["id"], "w1");
        let bad = json!({ "workflow": sample_wf("outro") });
        assert_eq!(call(addr, "PUT", "/v1/workflows/w1", t, Some(&bad)).0, 400);

        let (st, w) = call(addr, "GET", "/v1/workflows/w1", t, None);
        assert_eq!((st, w["name"].as_str()), (200, Some("teste")));
        assert_eq!(call(addr, "GET", "/v1/workflows/nada", t, None).0, 404);
        let (st, w) = call(
            addr,
            "POST",
            "/v1/workflows/w1/enabled",
            t,
            Some(&json!({"enabled": true})),
        );
        assert_eq!((st, w["enabled"].as_bool()), (200, Some(true)));

        let (st, r) = call(addr, "POST", "/v1/workflows/w1/run", t, None);
        assert_eq!(st, 202);
        let run_id = r["run_id"].as_str().unwrap().to_string();
        let mut run = Value::Null;
        for _ in 0..100 {
            run = call(addr, "GET", &format!("/v1/runs/{run_id}"), t, None).1;
            if run["status"] != "running" {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(run["status"], "ok", "{run}");
        assert!(run["node_logs"].as_array().is_some_and(|l| l.len() == 2));
        let (st, runs) = call(addr, "GET", "/v1/workflows/w1/runs?limit=5", t, None);
        assert_eq!((st, runs[0]["id"].as_str()), (200, Some(run_id.as_str())));
        assert_eq!(call(addr, "GET", "/v1/runs/nada", t, None).0, 404);

        let (st, h) = call(addr, "GET", "/v1/health", t, None);
        assert_eq!(st, 200);
        assert_eq!(
            (
                h["workflows_total"].as_u64(),
                h["workflows_enabled"].as_u64()
            ),
            (Some(1), Some(1))
        );
        assert!(h["daemon"].is_null(), "daemon nunca rodou");

        assert_eq!(call(addr, "DELETE", "/v1/workflows/w1", t, None).0, 200);
        assert_eq!(call(addr, "GET", "/v1/workflows/w1", t, None).0, 404);
    }

    #[test]
    fn health_mostra_o_sinal_de_vida_do_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        let store = zerith_workflows::Store::open(&dir.path().join("workflows.db")).unwrap();
        store
            .set_daemon_status(&zerith_workflows::DaemonStatus {
                version: "9.9.9".into(),
                started_at_ms: 1,
                last_cycle_ms: 99,
                hostname: "gw".into(),
                active_workflows: 2,
                next_run: None,
                consecutive_failures: Default::default(),
            })
            .unwrap();
        let h = call(addr, "GET", "/v1/health", Some(&tok), None).1;
        assert_eq!(h["daemon"]["last_cycle_ms"], json!(99));
        assert_eq!(h["daemon"]["hostname"], json!("gw"));
    }

    #[test]
    fn segredos_so_nomes_nunca_valores() {
        let dir = tempfile::tempdir().unwrap();
        let vault = MapSecrets::default();
        let addr = start_with(dir.path(), vault.clone());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        let t = Some(tok.as_str());
        let valor = "valor-super-secreto-123";
        let (st, v) = call(
            addr,
            "PUT",
            "/v1/secrets/API_X",
            t,
            Some(&json!({"value": valor})),
        );
        assert_eq!(st, 200);
        assert!(!v.to_string().contains(valor));
        assert_eq!(
            vault.0.lock().unwrap().get("API_X").map(String::as_str),
            Some(valor)
        );
        let (st, names) = call(addr, "GET", "/v1/secrets", t, None);
        assert_eq!((st, names), (200, json!(["API_X"])));
        assert_eq!(
            call(addr, "GET", "/v1/secrets/API_X", t, None).0,
            404,
            "sem leitura"
        );
        assert_eq!(
            call(
                addr,
                "PUT",
                "/v1/secrets/a%2F..",
                t,
                Some(&json!({"value": "x"}))
            )
            .0,
            400
        );
        for f in ["secrets.json", "tokens.json", "config.toml"] {
            let txt =
                std::fs::read_to_string(auth::server_dir(dir.path()).join(f)).unwrap_or_default();
            assert!(!txt.contains(valor) && !txt.contains(&tok), "{f}");
        }
        assert_eq!(call(addr, "DELETE", "/v1/secrets/API_X", t, None).0, 200);
        assert!(vault.0.lock().unwrap().is_empty());
        assert_eq!(call(addr, "GET", "/v1/secrets", t, None).1, json!([]));
    }

    #[test]
    fn cliente_remote_contra_o_servidor() {
        use zerith_workflows::remote::{PairingCode, RemoteClient, RemoteError};
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        let code = PairingCode {
            v: 1,
            name: "teste".into(),
            url: format!("http://{addr}"),
            token: tok,
        }
        .encode();
        let c = RemoteClient::from_pairing(&PairingCode::decode(&code).unwrap());
        assert_eq!(c.ping().unwrap(), zerith_core::VERSION);
        assert_eq!(c.health().unwrap().workflows_total, 0);
        let wf: zerith_workflows::Workflow = serde_json::from_value(sample_wf("w1")).unwrap();
        let saved = c.put(&wf, None).unwrap();
        assert!(matches!(c.put(&wf, None), Err(RemoteError::Conflict(_))));
        c.put(&saved, Some(saved.updated_at_ms)).unwrap();
        assert_eq!(c.list().unwrap().len(), 1);
        assert!(c.set_enabled("w1", true).unwrap().enabled);
        let run_id = c.run("w1", Some("x")).unwrap();
        for _ in 0..100 {
            if c.run_get(&run_id).unwrap().status != "running" {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(c.runs("w1", 5).unwrap()[0].status, "ok");
        c.secret_set("K", "v").unwrap();
        assert_eq!(c.secret_names().unwrap(), ["K"]);
        c.delete("w1").unwrap();
        assert!(matches!(c.get("w1"), Err(RemoteError::Status(404, _))));
        let bad = RemoteClient::new(&format!("http://{addr}"), "zt_errado");
        assert!(matches!(bad.list(), Err(RemoteError::Status(401, _))));
    }

    #[test]
    fn eventos_sse_entregam_run_iniciado_e_terminado() {
        use std::io::BufRead;
        let dir = tempfile::tempdir().unwrap();
        let addr = start(dir.path());
        let tok = auth::add_token(dir.path(), "pc").unwrap();
        let t = Some(tok.as_str());
        call(
            addr,
            "PUT",
            "/v1/workflows/w1",
            t,
            Some(&json!({ "workflow": sample_wf("w1") })),
        );

        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            s,
            "GET /v1/events HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n"
        )
        .unwrap();
        let mut rd = std::io::BufReader::new(s);
        let mut seen = String::new();
        // Cabeçalho + primeiro daemon_status (sai na hora).
        while !seen.contains("event: daemon_status") {
            rd.read_line(&mut seen).unwrap();
        }
        assert!(seen.starts_with("HTTP/1.1 200") && seen.contains("text/event-stream"));
        assert_eq!(call(addr, "POST", "/v1/workflows/w1/run", t, None).0, 202);
        while !seen.contains("event: run_finished") {
            assert!(rd.read_line(&mut seen).unwrap() > 0);
        }
        assert!(seen.contains("event: run_started"));
        assert!(!seen.contains(&tok));
        // Sem token não abre o fluxo.
        assert_eq!(call(addr, "GET", "/v1/events", None, None).0, 401);
    }
}
