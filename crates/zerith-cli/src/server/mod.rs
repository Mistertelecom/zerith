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
mod http;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_core::Paths;

type Boxed = Box<dyn std::error::Error>;

pub const DEFAULT_BIND: &str = "127.0.0.1:7878";
/// Conexões simultâneas (cada uma é uma thread; SSE conta enquanto aberto).
const MAX_CONNS: usize = 64;

/// Estado compartilhado por todas as conexões.
pub(crate) struct Ctx {
    pub root: PathBuf,
    pub started: Instant,
    conns: AtomicUsize,
    limiter: auth::FailLimiter,
    usage_lock: std::sync::Mutex<()>,
}

impl Ctx {
    pub(crate) fn new(paths: &Paths) -> Self {
        Self {
            root: paths.root().to_path_buf(),
            started: Instant::now(),
            conns: AtomicUsize::new(0),
            limiter: auth::FailLimiter::default(),
            usage_lock: std::sync::Mutex::new(()),
        }
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

fn handle_conn(ctx: &Ctx, mut stream: TcpStream) {
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
        Ok(()) => route(ctx, &req),
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

fn route(_ctx: &Ctx, req: &http::Request) -> (u16, Value) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/ping") => (
            200,
            json!({ "name": "zerith", "version": zerith_core::VERSION }),
        ),
        _ => (404, json!({ "error": "rota não encontrada" })),
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
    Ok(run_on(listener, Arc::new(Ctx::new(paths))))
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

    /// Sobe o servidor numa porta efêmera de 127.0.0.1 sobre uma raiz temporária.
    pub(crate) fn start(root: &std::path::Path) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        run_on(listener, Arc::new(Ctx::new(&Paths::with_root(root))));
        addr
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
        cmd(&paths, &a("server init --name casa --bind 100.64.0.9:7878")).unwrap();
        let cfg = auth::load_config(dir.path()).unwrap();
        assert_eq!(
            (cfg.name.as_str(), cfg.url.as_str()),
            ("casa", "http://100.64.0.9:7878")
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
}
