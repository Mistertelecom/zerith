//! Eventos do servidor (`run_started`, `run_finished`) num anel em memória e
//! `GET /v1/events` como Server-Sent Events. Com uma thread por conexão, SSE
//! é só não fechar a resposta: cada evento vira `id/event/data`, e a cada
//! 30 s sai um `daemon_status` (que também serve de keep-alive).
//! Reconexão: `?since=<id>` ou o cabeçalho `Last-Event-ID`.

use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Eventos guardados para quem reconecta com `since`.
const KEEP: usize = 500;
pub const STATUS_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct EventBus {
    /// (próximo id, eventos (id, tipo, dados)).
    log: Mutex<(u64, VecDeque<(u64, &'static str, Value)>)>,
    cv: Condvar,
}

impl EventBus {
    pub fn publish(&self, kind: &'static str, data: Value) {
        if let Ok(mut g) = self.log.lock() {
            g.0 += 1;
            let id = g.0;
            g.1.push_back((id, kind, data));
            if g.1.len() > KEEP {
                g.1.pop_front();
            }
        }
        self.cv.notify_all();
    }

    pub fn last_id(&self) -> u64 {
        self.log.lock().map_or(0, |g| g.0)
    }

    /// Eventos com id > `after`; espera até `timeout` se não houver nenhum.
    pub fn wait_after(&self, after: u64, timeout: Duration) -> Vec<(u64, &'static str, Value)> {
        let Ok(g) = self.log.lock() else {
            return Vec::new();
        };
        let Ok((g, _)) = self.cv.wait_timeout_while(g, timeout, |g| g.0 <= after) else {
            return Vec::new();
        };
        g.1.iter().filter(|e| e.0 > after).cloned().collect()
    }
}

/// Mantém a conexão aberta mandando eventos até o cliente sair.
pub fn serve_sse(
    stream: &mut TcpStream,
    bus: &EventBus,
    since: Option<u64>,
    status: &dyn Fn() -> Value,
) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\n\
                Cache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut last = since.unwrap_or_else(|| bus.last_id());
    let mut next_status = Instant::now();
    loop {
        if Instant::now() >= next_status {
            next_status = Instant::now() + STATUS_EVERY;
            let msg = format!("event: daemon_status\ndata: {}\n\n", status());
            if stream
                .write_all(msg.as_bytes())
                .and_then(|_| stream.flush())
                .is_err()
            {
                return;
            }
        }
        let wait = next_status.saturating_duration_since(Instant::now());
        for (id, kind, data) in bus.wait_after(last, wait) {
            last = id;
            let msg = format!("id: {id}\nevent: {kind}\ndata: {data}\n\n");
            if stream.write_all(msg.as_bytes()).is_err() {
                return;
            }
        }
        if stream.flush().is_err() {
            return;
        }
    }
}

pub fn run_event(run: &zerith_workflows::WorkflowRun) -> Value {
    json!({
        "run_id": run.id,
        "workflow_id": run.workflow_id,
        "status": run.status,
        "summary": run.summary,
        "finished_at_ms": run.finished_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anel_entrega_so_o_que_veio_depois_e_tem_limite() {
        let bus = EventBus::default();
        bus.publish("a", json!(1));
        bus.publish("b", json!(2));
        let got = bus.wait_after(1, Duration::ZERO);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, "b");
        assert!(bus.wait_after(2, Duration::from_millis(10)).is_empty());
        for i in 0..KEEP + 10 {
            bus.publish("x", json!(i));
        }
        assert_eq!(bus.wait_after(0, Duration::ZERO).len(), KEEP);
    }
}
