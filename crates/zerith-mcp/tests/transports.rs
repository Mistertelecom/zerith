//! Timeout over stdio and Streamable HTTP against an in-test server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
#[cfg(windows)]
use std::time::{Duration, Instant};

use zerith_mcp::Client;
#[cfg(windows)]
use zerith_mcp::McpError;

/// Answers `initialize`, then reads forever without answering.
#[cfg(windows)]
const SILENT: &str = r#"
$null = [Console]::In.ReadLine()
[Console]::Out.WriteLine('{"jsonrpc":"2.0","id":1,"result":{}}')
[Console]::Out.Flush()
while ($true) { $null = [Console]::In.ReadLine(); Start-Sleep -Seconds 60 }
"#;

#[test]
#[cfg(windows)]
fn a_silent_server_times_out_and_is_killed() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let script = dir.join(format!("mcp-silent-{}.ps1", std::process::id()));
    std::fs::write(&script, SILENT).unwrap();
    let args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
        .iter()
        .map(|s| s.to_string())
        .chain([script.display().to_string()])
        .collect();
    // Generous for powershell start-up; the hang is on the second request.
    let mut c = Client::connect_with_timeout("t", "powershell", &args, Duration::from_secs(30))
        .expect("connect");
    c.set_timeout(Duration::from_secs(2));
    let t = Instant::now();
    let err = c.tools().unwrap_err();
    assert!(matches!(err, McpError::Timeout(_)), "{err}");
    assert!(t.elapsed() < Duration::from_secs(10));
    // The child is dead, so the next request sees a closed stream, not a hang.
    assert!(c.tools().is_err());
    let _ = std::fs::remove_file(&script);
}

/// A cancelled call returns at once instead of waiting out the server
/// timeout, and kills the server.
#[test]
#[cfg(windows)]
fn a_cancelled_call_returns_without_waiting_for_the_timeout() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let script = dir.join(format!("mcp-cancel-{}.ps1", std::process::id()));
    std::fs::write(&script, SILENT).unwrap();
    let args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
        .iter()
        .map(|s| s.to_string())
        .chain([script.display().to_string()])
        .collect();
    let mut c = Client::connect_with_timeout("t", "powershell", &args, Duration::from_secs(30))
        .expect("connect");
    let t = Instant::now();
    let stop = move || t.elapsed() > Duration::from_millis(300);
    let err = c.call_until("x", serde_json::json!({}), &stop).unwrap_err();
    assert!(matches!(err, McpError::Cancelled), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(c.tools().is_err(), "the server was killed");
    let _ = std::fs::remove_file(&script);
}

/// One HTTP exchange: returns the request head+body, writes `reply`.
fn serve_one(listener: &TcpListener, reply: impl Fn(&str, &str) -> String) -> String {
    let (mut s, _) = listener.accept().unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
        }
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    let body = String::from_utf8(body).unwrap();
    s.write_all(reply(&head, &body).as_bytes()).unwrap();
    head + &body
}

fn http(status: &str, ctype: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn http_transport_handles_json_sse_and_session_id() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        // initialize: JSON body, hands out a session.
        seen.push(serve_one(&listener, |_, _| {
            http(
                "200 OK",
                "application/json",
                "Mcp-Session-Id: abc\r\n",
                r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            )
        }));
        // notifications/initialized: 202, no body.
        seen.push(serve_one(&listener, |_, _| {
            http("202 Accepted", "application/json", "", "")
        }));
        // tools/list: SSE with a notification before the answer.
        seen.push(serve_one(&listener, |_, _| {
            http(
                "200 OK",
                "text/event-stream",
                "",
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                 data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}\n\n",
            )
        }));
        seen
    });

    let mut c = Client::connect_http("h", &url).expect("connect");
    let tools = c.tools().expect("tools");
    assert_eq!(tools[0].name, "echo");
    assert_eq!(c.pid(), 0);

    let seen = server.join().unwrap();
    assert!(seen[0].contains("\"initialize\""));
    assert!(seen[0]
        .to_ascii_lowercase()
        .contains("accept: application/json, text/event-stream"));
    assert!(!seen[0].to_ascii_lowercase().contains("mcp-session-id"));
    assert!(seen[1].to_ascii_lowercase().contains("mcp-session-id: abc"));
    assert!(seen[2].to_ascii_lowercase().contains("mcp-session-id: abc"));
}
