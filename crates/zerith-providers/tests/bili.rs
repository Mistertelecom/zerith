//! Own test binary: the proxy origin is process-global, and here no other
//! test can be sent to it by accident.

use std::io::{Read, Write};
use std::net::TcpListener;

use zerith_providers::{
    bili, Cancel, ChatMessage, ChatRequest, Client, ProviderConfig, ProviderKind,
};

/// One request, answered with a canned completion; returns what was received.
fn fake_proxy() -> (String, std::thread::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    let h = std::thread::spawn(move || {
        // The first connection is the liveness probe (connect only).
        let mut got = String::new();
        for s in l.incoming().take(2) {
            let mut s = s.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then the body by its content-length.
            loop {
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if req.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            if req.is_empty() {
                continue;
            }
            got = String::from_utf8_lossy(&req).into_owned();
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
        got
    });
    (origin, h)
}

#[test]
fn a_chat_request_goes_to_the_proxy_with_the_upstream_in_the_path_and_the_conversation_id() {
    let (origin, h) = fake_proxy();
    bili::set_origin(Some(origin));
    let mut cfg = ProviderConfig::new(ProviderKind::OpenRouter).with_key("k");
    cfg.base_url = "https://openrouter.ai/api/v1".into();
    let client = Client::new(cfg).unwrap();
    let _c = bili::conversation("sess-1");
    let out = client
        .complete(
            &ChatRequest::new("m", vec![ChatMessage::user("oi")]),
            &Cancel::default(),
        )
        .unwrap();
    assert_eq!(out.text, "ok");
    let got = h.join().unwrap().to_ascii_lowercase();
    assert!(
        got.starts_with("post /bili/https://openrouter.ai/api/v1/chat/completions "),
        "{got}"
    );
    assert!(got.contains("x-acp-session: sess-1"), "{got}");
    assert!(
        got.contains("authorization: bearer k"),
        "the key rides through: {got}"
    );
}
