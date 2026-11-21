//! Remote execution targets (`ssh://[user@]host[:port][/path]` and
//! `docker://<container>[/path]`) for running CLI workspace sessions on remote
//! hosts or containers safely.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTarget {
    Ssh {
        user: Option<String>,
        host: String,
        port: Option<u16>,
        path: Option<String>,
    },
    Docker {
        container: String,
        path: Option<String>,
    },
}

fn validate_ident(field: &str, v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err(format!("{field} remoto não pode ficar vazio"));
    }
    if v.starts_with('-') {
        return Err(format!("{field} inválido (inicia com `-`): {v}"));
    }
    if v.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(
                c,
                ';' | '&' | '|' | '`' | '$' | '\'' | '"' | '<' | '>' | '\\'
            )
    }) {
        return Err(format!("{field} contém caractere inválido: {v}"));
    }
    Ok(())
}

fn validate_path(p: &str) -> Result<(), String> {
    if p.chars().any(|c| c.is_control() || matches!(c, '\0')) {
        return Err("caminho remoto contém caractere de controle".into());
    }
    Ok(())
}

/// Parses a `ssh://` or `docker://` target URI, resolving named aliases from
/// `[remote.targets]` first.
pub fn parse_remote_target(
    raw: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<RemoteTarget, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("--remote não pode ficar vazio".into());
    }
    let resolved = aliases.get(trimmed).map(String::as_str).unwrap_or(trimmed);

    if let Some(rest) = resolved.strip_prefix("ssh://") {
        let (auth_host, path) = match rest.split_once('/') {
            Some((a, p)) => (
                a,
                Some(format!("/{}", p.trim_end_matches('/'))).filter(|s| s != "/"),
            ),
            None => (rest, None),
        };
        if let Some(p) = &path {
            validate_path(p)?;
        }
        let (user, host_port) = match auth_host.split_once('@') {
            Some((u, hp)) => {
                validate_ident("usuário SSH", u)?;
                (Some(u.to_string()), hp)
            }
            None => (None, auth_host),
        };
        let (host, port) = match host_port.rsplit_once(':') {
            Some((h, p)) => {
                let n: u16 = p.parse().map_err(|_| format!("porta SSH inválida: {p}"))?;
                (h, Some(n))
            }
            None => (host_port, None),
        };
        validate_ident("host SSH", host)?;
        return Ok(RemoteTarget::Ssh {
            user,
            host: host.to_string(),
            port,
            path,
        });
    }

    if let Some(rest) = resolved.strip_prefix("docker://") {
        let (container, path) = match rest.split_once('/') {
            Some((c, p)) => (
                c,
                Some(format!("/{}", p.trim_end_matches('/'))).filter(|s| s != "/"),
            ),
            None => (rest, None),
        };
        validate_ident("container Docker", container)?;
        if let Some(p) = &path {
            validate_path(p)?;
        }
        return Ok(RemoteTarget::Docker {
            container: container.to_string(),
            path,
        });
    }

    Err(format!(
        "alvo remoto `{resolved}` inválido: use alias configurado ou URI ssh:// / docker://"
    ))
}

/// POSIX single-quote escaping (`'` -> `'\''`).
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Builds `(program, args)` to execute `command` inside `target`'s workspace.
pub fn wrap_remote_command(target: &RemoteTarget, command: &str) -> (String, Vec<String>) {
    match target {
        RemoteTarget::Ssh {
            user,
            host,
            port,
            path,
        } => {
            let mut args = vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=15".into(),
            ];
            if let Some(p) = port {
                args.push("-p".into());
                args.push(p.to_string());
            }
            let dest = match user {
                Some(u) => format!("{u}@{host}"),
                None => host.clone(),
            };
            args.push(dest);
            let script = match path {
                Some(dir) => format!("cd {} && {}", shell_quote(dir), command),
                None => command.to_string(),
            };
            args.push(script);
            ("ssh".into(), args)
        }
        RemoteTarget::Docker { container, path } => {
            let mut args = vec!["exec".into(), "-i".into()];
            if let Some(dir) = path {
                args.push("-w".into());
                args.push(dir.clone());
            }
            args.push(container.clone());
            args.push("sh".into());
            args.push("-c".into());
            args.push(command.to_string());
            ("docker".into(), args)
        }
    }
}

struct RemoteShellTool {
    spec: crate::ToolSpec,
    target: RemoteTarget,
    label: String,
}

impl crate::Tool for RemoteShellTool {
    fn spec(&self) -> &crate::ToolSpec {
        &self.spec
    }

    fn action(
        &self,
        args: &serde_json::Value,
        _ctx: &crate::CallCtx<'_>,
    ) -> crate::Result<Option<zerith_security::Action>> {
        let cmd = crate::fntool::str_arg(args, "command")?.trim();
        if cmd.is_empty() {
            return Err(crate::ToolError::BadArguments("command está vazio".into()));
        }
        Ok(Some(zerith_security::Action::Shell(format!(
            "remote({}): {cmd}",
            self.label
        ))))
    }

    fn run(
        &self,
        args: &serde_json::Value,
        _ctx: &crate::CallCtx<'_>,
        exec: &crate::Exec<'_>,
    ) -> crate::Result<String> {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let cmd = crate::fntool::str_arg(args, "command")?.trim();
        if cmd.is_empty() {
            return Err(crate::ToolError::BadArguments("command está vazio".into()));
        }
        let (prog, prog_args) = wrap_remote_command(&self.target, cmd);
        let mut child = Command::new(&prog)
            .args(&prog_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| crate::ToolError::Failed(format!("falha ao iniciar {prog}: {e}")))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_t = std::thread::spawn(move || -> Vec<u8> {
            let mut buf = Vec::new();
            if let Some(mut s) = stdout {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });
        let err_t = std::thread::spawn(move || -> Vec<u8> {
            let mut buf = Vec::new();
            if let Some(mut s) = stderr {
                let _ = s.read_to_end(&mut buf);
            }
            buf
        });

        let status = loop {
            match child.try_wait()? {
                Some(st) => break st,
                None => {
                    if let Err(err) = exec.check() {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = out_t.join();
                        let _ = err_t.join();
                        return Err(err);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        };

        let out_bytes = out_t.join().unwrap_or_default();
        let err_bytes = err_t.join().unwrap_or_default();
        let mut combined = String::from_utf8_lossy(&out_bytes).into_owned();
        let err_txt = String::from_utf8_lossy(&err_bytes);
        if !err_txt.trim().is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&err_txt);
        }
        if combined.len() > crate::OUTPUT_CAP_CHARS {
            let mut cut = crate::OUTPUT_CAP_CHARS;
            while cut > 0 && !combined.is_char_boundary(cut) {
                cut -= 1;
            }
            combined.truncate(cut);
            combined.push_str("\n[saída truncada]");
        }
        if status.success() {
            if combined.trim().is_empty() {
                Ok("(ok, sem saída)".into())
            } else {
                Ok(combined)
            }
        } else {
            let code = status
                .code()
                .map_or_else(|| "sinal".to_string(), |c| c.to_string());
            Err(crate::ToolError::Failed(format!(
                "remoto retornou código {code}\n{combined}"
            )))
        }
    }
}

/// Builds the session-scoped `remote_shell` tool targeting a validated SSH or
/// Docker endpoint.
pub fn remote_shell_tool(target: RemoteTarget, label: String) -> Box<dyn crate::Tool> {
    Box::new(RemoteShellTool {
        spec: crate::ToolSpec {
            name: "remote_shell".into(),
            description: format!("Executa comandos no alvo remoto ativo ({label}) via SSH/Docker."),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Comando shell a executar no workspace remoto"
                    }
                },
                "required": ["command"]
            }),
            returns: "saída stdout/stderr do comando remoto",
            permission: crate::Permission::Shell,
            timeout: std::time::Duration::from_secs(120),
            cancellable: true,
            local: false,
            source: "built-in".into(),
        },
        target,
        label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_docker_and_aliases_cleanly() {
        let mut aliases = BTreeMap::new();
        aliases.insert("prod".into(), "ssh://deploy@10.0.0.5:2222/srv/app".into());
        let t1 = parse_remote_target("prod", &aliases).unwrap();
        assert_eq!(
            t1,
            RemoteTarget::Ssh {
                user: Some("deploy".into()),
                host: "10.0.0.5".into(),
                port: Some(2222),
                path: Some("/srv/app".into()),
            }
        );
        let (prog, args) = wrap_remote_command(&t1, "cargo test");
        assert_eq!(prog, "ssh");
        assert!(args.contains(&"deploy@10.0.0.5".into()));
        assert!(args
            .last()
            .unwrap()
            .starts_with("cd '/srv/app' && cargo test"));

        let t2 = parse_remote_target("docker://api-ctr/workspace", &aliases).unwrap();
        assert_eq!(
            t2,
            RemoteTarget::Docker {
                container: "api-ctr".into(),
                path: Some("/workspace".into()),
            }
        );
        let (prog2, args2) = wrap_remote_command(&t2, "ls -la");
        assert_eq!(prog2, "docker");
        assert_eq!(
            args2,
            vec![
                "exec",
                "-i",
                "-w",
                "/workspace",
                "api-ctr",
                "sh",
                "-c",
                "ls -la"
            ]
        );
    }

    #[test]
    fn rejects_flag_and_shell_injection_and_quotes_safely() {
        let empty = BTreeMap::new();
        assert!(parse_remote_target("ssh://-oProxyCommand=id", &empty).is_err());
        assert!(parse_remote_target("ssh://evil;rm -rf /", &empty).is_err());
        assert!(parse_remote_target("docker://-privileged", &empty).is_err());
        assert!(parse_remote_target("http://bad", &empty).is_err());
        assert_eq!(shell_quote("it's ok; $(id)"), "'it'\\''s ok; $(id)'");
    }

    #[test]
    fn remote_shell_tool_gates_on_shell_action_and_rejects_empty_command() {
        let target = RemoteTarget::Docker {
            container: "lab-r1".into(),
            path: Some("/etc/frr".into()),
        };
        let tool = remote_shell_tool(target, "lab-r1".into());
        let pol = zerith_security::Policy::new(
            zerith_security::preset("workspace-write").unwrap(),
            zerith_security::Mode::Default,
            std::path::Path::new("."),
        );
        let ctx = crate::CallCtx {
            workspace: pol.workspace.as_path(),
            policy: &pol,
            memory: None,
            scope: None,
        };
        let act = tool
            .action(
                &serde_json::json!({"command": "vtysh -c 'show bgp summary'"}),
                &ctx,
            )
            .unwrap();
        assert_eq!(
            act,
            Some(zerith_security::Action::Shell(
                "remote(lab-r1): vtysh -c 'show bgp summary'".into()
            ))
        );
        assert!(tool
            .action(&serde_json::json!({"command": "   "}), &ctx)
            .is_err());
    }
}
