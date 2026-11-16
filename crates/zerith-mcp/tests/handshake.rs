//! The handshake is proven against a script that answers like a server, so the
//! suite needs no network and no installed MCP server.

#[cfg(windows)]
use zerith_mcp::Client;

#[cfg(windows)]
const SCRIPT: &str = r#"
$null = [Console]::In.ReadLine()
[Console]::Out.WriteLine('{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"t","version":"0"}}}')
[Console]::Out.Flush()
$null = [Console]::In.ReadLine()
$null = [Console]::In.ReadLine()
[Console]::Out.WriteLine('{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}}')
[Console]::Out.Flush()
"#;

#[test]
#[cfg(windows)]
fn the_handshake_lists_tools_without_a_network() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let script = dir.join(format!("mcp-{}.ps1", std::process::id()));
    std::fs::write(&script, SCRIPT).unwrap();
    let mut c = Client::connect(
        "test",
        "powershell",
        &[
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            script.display().to_string(),
        ],
    )
    .expect("connect");
    let tools = c.tools().expect("tools");
    assert_eq!(tools[0].name, "echo");
    assert!(c.pid() > 0);
    let _ = std::fs::remove_file(&script);
}
