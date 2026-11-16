//! The agent-loop adapter against an in-test Streamable HTTP server, so the
//! suite runs on every OS without an installed MCP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use serde_json::{json, Value};
use zerith_mcp::{McpRegistry, Origin, ServerConfig};
use zerith_security::{preset, Mode, Policy};
use zerith_tools::{CallCtx, Registry};

/// Reads one request and answers with `reply(body)`; returns the JSON body.
fn serve_one(l: &TcpListener, reply: impl Fn(&Value) -> (u16, String)) -> Value {
    let (mut s, _) = l.accept().unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
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
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    let req: Value = serde_json::from_slice(&body).unwrap();
    let (status, out) = reply(&req);
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
        out.len()
    );
    s.write_all(resp.as_bytes()).unwrap();
    req
}

fn ok(req: &Value, result: Value) -> (u16, String) {
    (
        200,
        json!({"jsonrpc":"2.0","id":req["id"],"result":result}).to_string(),
    )
}

fn config(url: String) -> ServerConfig {
    ServerConfig {
        name: "srv".into(),
        command: None,
        args: vec![],
        url: Some(url),
        timeout_ms: Some(5000),
        origin: Origin::User,
    }
}

fn ctx_parts() -> (std::path::PathBuf, Policy) {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let policy = Policy::new(preset("workspace-write").unwrap(), Mode::Bypass, &dir);
    (dir, policy)
}

#[test]
fn server_tools_register_with_their_schema_and_calls_reach_the_server() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        seen.push(serve_one(&l, |r| ok(r, json!({}))));
        seen.push(serve_one(&l, |_| (202, String::new())));
        seen.push(serve_one(&l, |r| {
            ok(
                r,
                json!({"tools":[
                    {"name":"echo tool","description":"ecoa","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},
                    {"name":"echo_tool"}
                ]}),
            )
        }));
        seen.push(serve_one(&l, |r| {
            ok(
                r,
                json!({"content":[{"type":"text","text":format!("eco: {}", r["params"]["arguments"]["text"])}]}),
            )
        }));
        seen.push(serve_one(&l, |r| {
            ok(
                r,
                json!({"content":[{"type":"text","text":"falhou"}],"isError":true}),
            )
        }));
        seen
    });

    let mcp = McpRegistry::new(vec![config(url)]);
    let mut reg = Registry::empty();
    let done = mcp.register_into(&mut reg);
    // `echo tool` and `echo_tool` sanitise to the same name: the second is
    // refused loudly instead of shadowing the first.
    assert_eq!(done.tools, ["mcp__srv__echo_tool"]);
    assert_eq!(done.errors.len(), 1, "{:?}", done.errors);
    assert!(done.errors[0].contains("colide"));

    let spec = reg.get("mcp__srv__echo_tool").unwrap();
    assert_eq!(spec.description, "ecoa");
    assert_eq!(spec.parameters["required"][0], "text");
    assert_eq!(spec.source, "mcp:srv");
    assert!(!spec.local);

    let (dir, policy) = ctx_parts();
    let ctx = CallCtx {
        workspace: &dir,
        policy: &policy,
        memory: None,
        scope: None,
    };
    let out = reg
        .call("mcp__srv__echo_tool", &json!({"text": "oi"}), &ctx)
        .unwrap();
    assert_eq!(out, "eco: \"oi\"");
    // isError becomes a tool error, so the model is told the call failed.
    let err = reg
        .call("mcp__srv__echo_tool", &json!({"text": "x"}), &ctx)
        .unwrap_err();
    assert_eq!(err.to_string(), "falhou");

    let seen = server.join().unwrap();
    assert_eq!(seen[3]["params"]["name"], "echo tool");
}

#[test]
fn an_unreachable_server_is_reported_and_does_not_block_the_rest() {
    // Bind then drop: the port is free, so the connection is refused.
    let url = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/mcp", l.local_addr().unwrap())
    };
    let mcp = McpRegistry::new(vec![config(url)]);
    let mut reg = Registry::empty();
    let done = mcp.register_into(&mut reg);
    assert!(done.tools.is_empty());
    assert_eq!(done.errors.len(), 1);
    assert!(done.errors[0].starts_with("servidor MCP srv:"));
    assert!(reg.specs().is_empty());
}

#[test]
fn resources_and_prompts_are_reachable_for_mentions_and_slash_commands() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", l.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        serve_one(&l, |r| ok(r, json!({})));
        serve_one(&l, |_| (202, String::new()));
        // resources/list (name -> uri), then resources/read.
        serve_one(&l, |r| {
            ok(r, json!({"resources":[{"uri":"doc://guia","name":"guia"}]}))
        });
        serve_one(&l, |r| {
            assert_eq!(r["params"]["uri"], "doc://guia");
            ok(
                r,
                json!({"contents":[{"uri":"doc://guia","text":"conteúdo do guia"}]}),
            )
        });
        // prompts/list, prompts/get.
        serve_one(&l, |r| {
            ok(
                r,
                json!({"prompts":[{"name":"resumo","description":"resume","arguments":[{"name":"tema","required":true}]}]}),
            )
        });
        serve_one(&l, |r| {
            ok(
                r,
                json!({"messages":[{"role":"user","content":{"type":"text","text":"resuma redes"}}]}),
            )
        });
    });

    let mcp = McpRegistry::new(vec![config(url)]);
    let (text, errors) = mcp.expand_mentions("leia @srv:guia e @naoexiste:x");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(text.starts_with("leia @srv:guia e @naoexiste:x"));
    assert!(text.contains("<recurso servidor=\"srv\" id=\"guia\">\nconteúdo do guia\n</recurso>"));

    let cmds = mcp.prompt_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command, "/mcp__srv__resumo");
    assert_eq!(cmds[0].arguments, [("tema".to_string(), true)]);
    assert_eq!(
        mcp.get_prompt("srv", "resumo", json!({"tema": "redes"}))
            .unwrap(),
        "[user] resuma redes"
    );
    server.join().unwrap();
}
