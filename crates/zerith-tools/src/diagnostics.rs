//! `diagnostics`: runs the project's own checker (cargo check, tsc, go vet,
//! ruff / py_compile) and hands back `file:line:col: erro|aviso: message`
//! lines, so the model does not have to guess the command or read a build log.
//!
//! The process itself goes through the built-in `shell` tool: same jail,
//! decoding, timeout and cancel, and the policy judges the real command line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use regex::Regex;
use serde_json::{json, Value};
use zerith_security::Action;

use crate::fntool::{make_with, opt_str};
use crate::{CallCtx, Exec, Permission, Result, Tool, ToolError};

const MAX_LINES: usize = 200;

#[derive(Debug, PartialEq)]
struct Diag {
    file: String,
    line: u32,
    col: u32,
    warn: bool,
    msg: String,
}

#[derive(Debug)]
struct Plan {
    dir: PathBuf,
    label: String,
    command: String,
}

fn args_of(args: &Value, ctx: &CallCtx<'_>) -> Result<(Plan, Vec<String>)> {
    let files: Vec<String> = args["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str())
                // Absolute, so the checker finds them from its own folder.
                .map(|f| ctx.resolve(f).to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let start = opt_str(args, "path")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.workspace.to_path_buf());
    Ok((plan(&start, &files, opt_str(args, "command"))?, files))
}

/// Walks up from `start` to the nearest project marker; `command` overrides.
fn plan(start: &Path, files: &[String], command: Option<&str>) -> Result<Plan> {
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    if !start.is_dir() {
        return Err(ToolError::BadArguments(format!(
            "pasta não encontrada: {}",
            start.display()
        )));
    }
    let at = |label: &str, command: String| Plan {
        dir: start.to_path_buf(),
        label: label.into(),
        command,
    };
    if let Some(c) = command {
        return Ok(at(c, c.into()));
    }
    for dir in start.ancestors() {
        if let Some(p) = detect(dir, files) {
            return Ok(p);
        }
    }
    if files.iter().any(|f| f.ends_with(".py")) {
        let (label, command) = python(files);
        return Ok(at(label, command));
    }
    Err(ToolError::BadArguments(format!(
        "nenhum projeto reconhecido (Cargo.toml, tsconfig.json, go.mod, pyproject.toml, package.json com lint/typecheck) subindo de {}; passe `command` com o verificador",
        start.display()
    )))
}

fn detect(dir: &Path, files: &[String]) -> Option<Plan> {
    let has = |f: &str| dir.join(f).is_file();
    let (label, command) = if has("Cargo.toml") {
        (
            "cargo check",
            "cargo check --all-targets --message-format=short --quiet".to_string(),
        )
    } else if has("tsconfig.json") {
        (
            "tsc",
            "npx --no-install tsc --noEmit --pretty false -p .".to_string(),
        )
    } else if has("go.mod") {
        ("go vet", "go vet ./...".to_string())
    } else if has("pyproject.toml") || has("setup.py") || has_py(dir) {
        python(files)
    } else {
        let pkg: Value =
            serde_json::from_slice(&std::fs::read(dir.join("package.json")).ok()?).ok()?;
        let script = ["typecheck", "lint"]
            .into_iter()
            .find(|s| pkg["scripts"][s].is_string())?;
        ("npm run", format!("npm run -s {script}"))
    };
    Some(Plan {
        dir: dir.to_path_buf(),
        label: label.into(),
        command,
    })
}

fn has_py(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "py"))
    })
}

fn python(files: &[String]) -> (&'static str, String) {
    let py = if cfg!(windows) { "python" } else { "python3" };
    let picked: Vec<String> = files
        .iter()
        .filter(|f| f.ends_with(".py"))
        // `/` reads the same in cmd and bash; `\` does not.
        .map(|f| format!("\"{}\"", f.replace('\\', "/")))
        .collect();
    if on_path("ruff") {
        ("ruff", "ruff check --output-format=concise".into())
    } else if !picked.is_empty() {
        (
            "py_compile",
            format!("{py} -m py_compile {}", picked.join(" ")),
        )
    } else {
        (
            "compileall",
            format!("{py} -m compileall -q -x \"(\\.git|venv|node_modules|site-packages)\" ."),
        )
    }
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p)
            .any(|d| d.join(name).is_file() || d.join(format!("{name}.exe")).is_file())
    })
}

/// Every location-bearing line of the known formats: `file:line:col: msg`
/// (cargo short, go vet, ruff concise, eslint unix, mypy), `file(line,col):`
/// (tsc) and Python's `File "x", line N` + `XxxError: msg`.
fn parse(out: &str) -> Vec<Diag> {
    let loc =
        Regex::new(r"^(?:vet: )?((?:[A-Za-z]:)?[^:(\s][^:(]*?):(\d+):(?:(\d+):)?\s+(.+)$").unwrap();
    let tsc = Regex::new(r"^(.+?)\((\d+),(\d+)\): (error|warning) (.+)$").unwrap();
    let kind = Regex::new(r"(?i)^(error|warning|note|help)(?:\[([^\]]+)\])?:\s*(.*)$").unwrap();
    let py_at = Regex::new(r#"^\s*File "(.+)", line (\d+)"#).unwrap();
    let py_err = Regex::new(r"^(\w*(?:Error|Warning)): (.+)$").unwrap();
    let num = |s: Option<regex::Match>| s.and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
    let mut v = Vec::new();
    let mut py: Option<(String, u32)> = None;
    for line in out.lines().map(str::trim_end) {
        if let Some(c) = tsc.captures(line) {
            v.push(Diag {
                file: c[1].into(),
                line: num(c.get(2)),
                col: num(c.get(3)),
                warn: &c[4] == "warning",
                msg: c[5].into(),
            });
        } else if let Some(c) = loc.captures(line) {
            let (warn, msg) = match kind.captures(&c[4]) {
                Some(k) => match k[1].to_ascii_lowercase().as_str() {
                    "note" | "help" => continue,
                    w => (
                        w == "warning",
                        match k.get(2) {
                            Some(code) => format!("{}: {}", code.as_str(), &k[3]),
                            None => k[3].to_string(),
                        },
                    ),
                },
                None => (false, c[4].to_string()),
            };
            v.push(Diag {
                file: c[1].into(),
                line: num(c.get(2)),
                col: num(c.get(3)),
                warn,
                msg,
            });
        } else if let Some(c) = py_at.captures(line) {
            py = Some((c[1].into(), num(c.get(2))));
        } else if let (Some(c), Some((file, l))) = (py_err.captures(line), py.as_ref()) {
            // ponytail: py_compile prints no column, only a caret under a
            // re-indented line; col 1 until a checker with columns is used.
            v.push(Diag {
                file: file.clone(),
                line: *l,
                col: 1,
                warn: c[1].ends_with("Warning"),
                msg: format!("{}: {}", &c[1], &c[2]),
            });
            py = None;
        }
    }
    v
}

/// The path as the model should see it: relative to the workspace, `/`.
/// A checker's relative path is relative to its own root (cargo prints from
/// the cargo workspace, not from the crate), so the first ancestor of `dir`
/// where the file exists wins.
fn rel(file: &str, dir: &Path, ws: &Path) -> String {
    let p = Path::new(file.trim_start_matches("./").trim_start_matches(".\\"));
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.ancestors()
            .map(|b| b.join(p))
            .find(|c| c.exists())
            .unwrap_or_else(|| dir.join(p))
    };
    abs.strip_prefix(ws)
        .unwrap_or(&abs)
        .to_string_lossy()
        .replace('\\', "/")
}

fn count(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn report(
    out: &str,
    ok: bool,
    plan: &Plan,
    ws: &Path,
    files: &[String],
    secs: f64,
) -> Result<String> {
    let all = parse(out);
    if !ok && !all.iter().any(|d| !d.warn) {
        // Failed without one parseable error (missing tool, bad manifest,
        // linker): the raw tail is the only diagnosis there is.
        let raw: Vec<&str> = out.lines().collect();
        let tail = raw[raw.len().saturating_sub(40)..].join("\n");
        return Err(ToolError::Failed(format!(
            "o verificador falhou sem erro reconhecível (`{}` em {}); confira o comando ou passe `command`:\n{tail}",
            plan.command,
            plan.dir.display()
        )));
    }
    let wanted: Vec<String> = files.iter().map(|f| rel(f, ws, ws)).collect();
    let mut seen = HashSet::new();
    let mut lines: Vec<(bool, String)> = Vec::new();
    let mut elsewhere = 0;
    for d in &all {
        let file = rel(&d.file, &plan.dir, ws);
        if !wanted.is_empty() && !wanted.contains(&file) {
            elsewhere += 1;
            continue;
        }
        let sev = if d.warn { "aviso" } else { "erro" };
        let text = format!("{file}:{}:{}: {sev}: {}", d.line, d.col, d.msg);
        if seen.insert(text.clone()) {
            lines.push((d.warn, text));
        }
    }
    // Errors first, each group in the checker's order.
    lines.sort_by_key(|(warn, _)| *warn);
    let errors = lines.iter().filter(|(w, _)| !w).count();
    let mut text = format!(
        "{}, {} ({}, {secs:.1} s)",
        count(errors, "erro"),
        count(lines.len() - errors, "aviso"),
        plan.label
    );
    for (_, l) in lines.iter().take(MAX_LINES) {
        text.push('\n');
        text.push_str(l);
    }
    if lines.len() > MAX_LINES {
        text.push_str(&format!(
            "\n… mais {} linhas omitidas; filtre com `files`",
            lines.len() - MAX_LINES
        ));
    }
    if elsewhere > 0 {
        text.push_str(&format!("\n({elsewhere} diagnósticos em outros arquivos)"));
    }
    Ok(text)
}

fn action(args: &Value, ctx: &CallCtx<'_>) -> Result<Option<Action>> {
    Ok(Some(Action::Shell(args_of(args, ctx)?.0.command)))
}

fn run(args: &Value, ctx: &CallCtx<'_>, exec: &Exec<'_>) -> Result<String> {
    let (plan, files) = args_of(args, ctx)?;
    let shell = crate::builtin::all()
        .into_iter()
        .find(|t| t.spec().name == "shell")
        .ok_or_else(|| ToolError::Failed("ferramenta shell ausente".into()))?;
    let sub = CallCtx {
        workspace: &plan.dir,
        policy: ctx.policy,
        memory: None,
        scope: None,
    };
    let started = Instant::now();
    let left = exec.deadline.saturating_duration_since(started).as_secs();
    let out = match shell.run(
        &json!({"command": plan.command, "timeout_secs": left.max(1)}),
        &sub,
        exec,
    ) {
        Ok(t) => t,
        Err(ToolError::Failed(t)) if t.starts_with("exit ") => t,
        Err(e) => return Err(e),
    };
    let secs = started.elapsed().as_secs_f64();
    report(
        &out,
        out.starts_with("exit 0\n"),
        &plan,
        ctx.workspace,
        &files,
        secs,
    )
}

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![make_with(
        "diagnostics",
        "Roda o verificador do projeto (cargo check, tsc, go vet, ruff ou py_compile, detectado pela pasta) e devolve os erros e avisos como `arquivo:linha:coluna: erro|aviso: mensagem`, erros primeiro. Rode depois de editar código e antes de dizer que terminou; corrija e rode de novo até dar 0 erros.",
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "pasta ou arquivo de onde subir até achar o projeto (padrão: workspace)"},
            "files": {"type": "array", "items": {"type": "string"}, "description": "mostra só os diagnósticos destes arquivos"},
            "command": {"type": "string", "description": "verificador a rodar no lugar do detectado (ex.: \"mypy --show-column-numbers .\")"}
        }}),
        "contagem no topo (\"3 erros, 2 avisos (cargo check, 4.1 s)\") e uma linha arquivo:linha:coluna: erro|aviso: mensagem por diagnóstico",
        Permission::Shell,
        300,
        false,
        action,
        run,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(file: &str, line: u32, col: u32, warn: bool, msg: &str) -> Diag {
        Diag {
            file: file.into(),
            line,
            col,
            warn,
            msg: msg.into(),
        }
    }

    #[test]
    fn cargo_short() {
        let out = "exit 101\n--- stderr\n\
src\\main.rs:2:18: error[E0308]: mismatched types: expected `u32`, found `&str`\n\
src\\main.rs:3:9: warning: unused variable: `unused`: help: if this is intentional, prefix it with an underscore: `_unused`\n\
error: could not compile `diag-spike` (bin \"diag-spike\") due to 1 previous error\n\
src\\main.rs:2:18: error[E0308]: mismatched types: expected `u32`, found `&str`\n";
        let v = parse(out);
        assert_eq!(
            v[0],
            d(
                "src\\main.rs",
                2,
                18,
                false,
                "E0308: mismatched types: expected `u32`, found `&str`"
            )
        );
        assert!(v[1].warn && v[1].msg.starts_with("unused variable"));
        assert_eq!(v.len(), 3, "the could-not-compile line has no location");
    }

    #[test]
    fn tsc_pretty_false() {
        let v = parse("src/App.tsx(12,7): error TS2322: Type 'string' is not assignable to type 'number'.\r\nFound 1 error.");
        assert_eq!(
            v,
            vec![d(
                "src/App.tsx",
                12,
                7,
                false,
                "TS2322: Type 'string' is not assignable to type 'number'."
            )]
        );
    }

    #[test]
    fn go_vet() {
        let v = parse("# example.com/m\n# [example.com/m]\nvet: ./main.go:8:2: fmt.Printf format %d has arg s of wrong type string\n./util.go:4:2: undefined: x\n");
        assert_eq!(
            v,
            vec![
                d(
                    "./main.go",
                    8,
                    2,
                    false,
                    "fmt.Printf format %d has arg s of wrong type string"
                ),
                d("./util.go", 4, 2, false, "undefined: x"),
            ]
        );
    }

    #[test]
    fn ruff_concise_and_python() {
        let v = parse("bad.py:1:7: invalid-syntax: Expected `)`, found newline\nlint.py:1:8: F401 [*] `os` imported but unused\nFound 2 errors.\n[*] 1 fixable with `--fix`.");
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[1],
            d("lint.py", 1, 8, false, "F401 [*] `os` imported but unused")
        );
        let v = parse("exit 1\n--- stderr\n  File \"bad.py\", line 3\r\n    def f(:\r\n          ^\r\nSyntaxError: invalid syntax\r\n");
        assert_eq!(
            v,
            vec![d("bad.py", 3, 1, false, "SyntaxError: invalid syntax")]
        );
    }

    #[test]
    fn report_puts_errors_first_dedupes_and_filters() {
        let ws = Path::new("C:/ws");
        let plan = Plan {
            dir: ws.join("app"),
            label: "cargo check".into(),
            command: String::new(),
        };
        let out = "exit 101\nsrc/a.rs:1:1: warning: w\nsrc/a.rs:2:2: error: e\nsrc/a.rs:2:2: error: e\nsrc/b.rs:3:3: error: other\n";
        let r = report(out, false, &plan, ws, &[], 1.25).unwrap();
        assert_eq!(
            r,
            "2 erros, 1 aviso (cargo check, 1.2 s)\napp/src/a.rs:2:2: erro: e\napp/src/b.rs:3:3: erro: other\napp/src/a.rs:1:1: aviso: w"
        );
        let r = report(out, false, &plan, ws, &["app/src/a.rs".into()], 1.0).unwrap();
        assert!(r.starts_with("1 erro, 1 aviso"), "{r}");
        assert!(r.ends_with("(1 diagnósticos em outros arquivos)"), "{r}");
        let clean = report("exit 0\n", true, &plan, ws, &[], 0.5).unwrap();
        assert_eq!(clean, "0 erros, 0 avisos (cargo check, 0.5 s)");
        let broken = report(
            "exit 1\n--- stderr\n'tsc' não é reconhecido",
            false,
            &plan,
            ws,
            &[],
            0.1,
        )
        .unwrap_err()
        .to_string();
        assert!(broken.contains("não é reconhecido"), "{broken}");
    }

    #[test]
    fn detects_the_nearest_project() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&base).unwrap();
        let tmp = tempfile::Builder::new().tempdir_in(base).unwrap();
        let rust = tmp.path().join("rust");
        std::fs::create_dir_all(rust.join("src")).unwrap();
        std::fs::write(rust.join("Cargo.toml"), "[package]").unwrap();
        let p = plan(&rust.join("src"), &[], None).unwrap();
        assert_eq!(
            (p.label.as_str(), p.dir.as_path()),
            ("cargo check", rust.as_path())
        );
        assert!(p.command.contains("--all-targets"));
        let ts = tmp.path().join("ts");
        std::fs::create_dir_all(&ts).unwrap();
        std::fs::write(ts.join("tsconfig.json"), "{}").unwrap();
        let p = plan(&ts, &[], None).unwrap();
        assert_eq!(p.label, "tsc");
        assert!(p.command.starts_with("npx --no-install tsc --noEmit"));
        let p = plan(&ts, &[], Some("mypy .")).unwrap();
        assert_eq!(p.command, "mypy .");
        let py = rust.join("scripts");
        std::fs::create_dir_all(&py).unwrap();
        std::fs::write(py.join("a.py"), "x = 1").unwrap();
        let p = plan(&py, &[], None).unwrap();
        assert_eq!(p.dir, py, "loose .py files beat a Cargo.toml further up");
    }
}
