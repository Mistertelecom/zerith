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
}

impl Ctx {
    pub(crate) fn new(paths: &Paths) -> Self {
        Self {
            root: paths.root().to_path_buf(),
            started: Instant::now(),
            conns: AtomicUsize::new(0),
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
    let (status, body) = route(ctx, &req, peer);
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

fn route(_ctx: &Ctx, req: &http::Request, _peer: Option<SocketAddr>) -> (u16, Value) {
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
pub fn spawn(paths: &Paths) -> Result<JoinHandle<()>, Boxed> {
    let addr: SocketAddr = DEFAULT_BIND.parse()?;
    let listener = TcpListener::bind(addr)?;
    eprintln!("zerith server ouvindo em http://{}", listener.local_addr()?);
    Ok(run_on(listener, Arc::new(Ctx::new(paths))))
}

/// `zerith server ...`
pub fn cmd(paths: &Paths, args: &[String]) -> Result<(), Boxed> {
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let h = spawn(paths)?;
            h.join()
                .map_err(|_| "thread do servidor terminou com pânico")?;
            Ok(())
        }
        _ => Err("uso: zerith server serve".into()),
    }
}
