//! Diagnóstico depois de gravar: roda um verificador rápido de linha de
//! comando no arquivo antes e depois da edição e devolve só os erros que a
//! edição criou. Erro que já existia não volta para o modelo como se fosse
//! dele.
//!
//! Ideias de: Hermes `tools/file_operations_lint.py` (`_check_lint_delta`,
//! MIT: só os erros novos, o "antes" sem lint conta como limpo) e opencode
//! v1.19.6 `lsp/diagnostic.ts` + `tool/edit.ts` (MIT: só erros, no máximo 20
//! por arquivo). A doc do opencode recomenda CLI em vez de LSP para isso.
//!
//! Os dois lados rodam numa cópia em pasta temporária com o mesmo nome de
//! arquivo: saída simétrica, nada (`.pyc`, `.ruff_cache`) cai no workspace.
//! `.rs`, `.ts`, `.go` ficam de fora: compilador inteiro por arquivo é lento;
//! isso é do LSP aquecido e do verify-on-stop.
//! ponytail: a comparação ignora números (linha mudou de lugar = mesmo erro);
//! dois erros de mesma mensagem trocados de lugar passam como velhos. Mapa
//! de deslocamento de linhas entra se isso aparecer na prática.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;

const MAX_ERRORS: usize = 20;
const BUDGET: Duration = Duration::from_secs(10);
const PREFIX: &str = "[diagnóstico após gravar]";

#[derive(Debug, Clone, PartialEq)]
struct Diag {
    line: u32,
    msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Checker {
    Python,
    Ruff,
    Bash,
    ShellCheck,
    Node,
    Pwsh,
}

/// Em ordem: o primeiro que acusa algo encerra (sintaxe antes de lint, sem
/// o mesmo erro de sintaxe repetido por dois verificadores). Para Python,
/// `ruff` já verifica sintaxe (`E9`) e nomes indefinidos (`F82`) em um único
/// processo nativo; `python` só entra como fallback se `ruff` não estiver instalado.
fn checkers(path: &Path) -> &'static [Checker] {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "py" => &[Checker::Ruff, Checker::Python],
        "sh" | "bash" => &[Checker::Bash, Checker::ShellCheck],
        "js" | "mjs" | "cjs" => &[Checker::Node],
        "ps1" | "psm1" => &[Checker::Pwsh],
        _ => &[],
    }
}

// `compile()` em vez de `python -m py_compile`: mesma checagem, sem gravar `.pyc`.
const PY_SCRIPT: &str = "import sys\np=sys.argv[1]\ntry:\n    compile(open(p,'rb').read(),p,'exec',dont_inherit=True)\nexcept SyntaxError as e:\n    print(f'{e.lineno or 1}: {type(e).__name__}: {e.msg}')";

impl Checker {
    fn program(self) -> Option<PathBuf> {
        let names: &[&str] = match self {
            Checker::Python if cfg!(windows) => &["python", "python3"],
            Checker::Python => &["python3", "python"],
            Checker::Ruff => &["ruff"],
            // No Windows, `bash` do PATH pode ser o do WSL (outra máquina).
            Checker::Bash if cfg!(windows) => {
                return zerith_core::shellparse::windows_bash().map(Path::to_path_buf)
            }
            Checker::Bash => &["bash"],
            Checker::ShellCheck => &["shellcheck"],
            Checker::Node => &["node"],
            Checker::Pwsh => &["pwsh"],
        };
        names.iter().map(PathBuf::from).find(|p| available(p))
    }

    fn command(self, prog: &Path, file: &Path) -> Command {
        let mut c = Command::new(prog);
        // Git Bash e os demais leem `/` em qualquer SO.
        let f = file.to_string_lossy().replace('\\', "/");
        match self {
            Checker::Python => c.args(["-I", "-X", "utf8", "-c", PY_SCRIPT, &f]),
            Checker::Ruff => c.args([
                "check",
                "--output-format=concise",
                "--select",
                "E9,F63,F7,F82",
                "--no-cache",
                "--isolated",
                &f,
            ]),
            Checker::Bash => c.args(["-n", &f]),
            // SC2148 (sem shebang) é estilo, não erro: não acusar todo script sem `#!`.
            Checker::ShellCheck => c.args(["-f", "gcc", "-S", "error", "-e", "SC2148", &f]),
            Checker::Node => c.args(["--check", &f]),
            Checker::Pwsh => c.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;$e=$null;[void][System.Management.Automation.Language.Parser]::ParseFile('{}',[ref]$null,[ref]$e);foreach($x in $e){{'{{0}}: {{1}}' -f $x.Extent.StartLineNumber,$x.Message}}",
                    f.replace('\'', "''")
                ),
            ]),
        };
        c
    }

    fn parse(self, out: &str) -> Vec<Diag> {
        static RE_COLON_PREFIX: OnceLock<Regex> = OnceLock::new();
        static RE_LINE_COL: OnceLock<Regex> = OnceLock::new();
        static RE_BASH_LINE: OnceLock<Regex> = OnceLock::new();
        static RE_NODE_LINE: OnceLock<Regex> = OnceLock::new();
        static RE_NODE_ERR: OnceLock<Regex> = OnceLock::new();

        let pick = |r: &Regex, skip: fn(&str) -> bool| -> Vec<Diag> {
            out.lines()
                .filter_map(|l| r.captures(l.trim_end()))
                .filter_map(|c| {
                    let msg = c[2].trim().to_string();
                    (!skip(&msg)).then(|| Diag {
                        line: c[1].parse().unwrap_or(1),
                        msg,
                    })
                })
                .collect()
        };
        match self {
            Checker::Python | Checker::Pwsh => pick(
                RE_COLON_PREFIX.get_or_init(|| Regex::new(r"^(\d+): (.+)$").expect("regex fixa")),
                |_| false,
            ),
            // ruff concise e shellcheck gcc: `arquivo:L:C: msg`.
            Checker::Ruff | Checker::ShellCheck => pick(
                RE_LINE_COL.get_or_init(|| Regex::new(r":(\d+):\d+: (.+)$").expect("regex fixa")),
                |_| false,
            ),
            // A linha que repete o código vem entre crases: não é erro.
            Checker::Bash => pick(
                RE_BASH_LINE
                    .get_or_init(|| Regex::new(r": line (\d+): (.+)$").expect("regex fixa")),
                |m| m.starts_with('`'),
            ),
            // `arquivo:L`, o trecho, o `^`, e `SyntaxError: msg`.
            Checker::Node => {
                let re_line =
                    RE_NODE_LINE.get_or_init(|| Regex::new(r":(\d+)$").expect("regex fixa"));
                let re_err =
                    RE_NODE_ERR.get_or_init(|| Regex::new(r"^\w*Error: ").expect("regex fixa"));
                let line = out
                    .lines()
                    .find_map(|l| re_line.captures(l.trim_end()))
                    .and_then(|c| c[1].parse().ok());
                let msg = out
                    .lines()
                    .find(|l| re_err.is_match(l))
                    .map(|l| l.trim().to_string());
                match (line, msg) {
                    (Some(line), Some(msg)) => vec![Diag { line, msg }],
                    _ => Vec::new(),
                }
            }
        }
    }
}

/// `--version` respondeu? Uma vez por programa (o atalho da Microsoft Store
/// para `python` existe no PATH e só abre a loja).
fn available(prog: &Path) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(&hit) = cache.lock().ok().as_ref().and_then(|m| m.get(prog)) {
        return hit;
    }
    let ok = spawn(
        Command::new(prog).arg("--version"),
        Stdio::null(),
        Stdio::null(),
    )
    .and_then(|c| wait(c, Instant::now() + Duration::from_secs(5)))
    .is_some_and(|s| s.success());
    if let Ok(mut m) = cache.lock() {
        m.insert(prog.to_path_buf(), ok);
    }
    ok
}

fn spawn(cmd: &mut Command, out: Stdio, err: Stdio) -> Option<Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .ok()
}

/// Espera até `deadline`; estourou, mata e devolve `None`.
fn wait(mut child: Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Erros de `bytes` como se fossem o arquivo `name`. `None`: nenhum
/// verificador rodou até o fim (ausente ou tempo esgotado).
fn diags(name: &Path, bytes: &[u8], deadline: Instant) -> Option<Vec<Diag>> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("zerith-postcheck-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let res = (|| {
        let file = dir.join(name.file_name()?);
        std::fs::write(&file, bytes).ok()?;
        let log = dir.join("saida.log");
        let mut ran = false;
        for &c in checkers(name) {
            let Some(prog) = c.program() else { continue };
            let f = std::fs::File::create(&log).ok()?;
            let child = spawn(
                c.command(&prog, &file).current_dir(&dir),
                f.try_clone().ok()?.into(),
                f.into(),
            )?;
            wait(child, deadline)?;
            ran = true;
            let found = c.parse(&String::from_utf8_lossy(&std::fs::read(&log).ok()?));
            if !found.is_empty() {
                return Some(found);
            }
            // Ruff already covers Python syntax (E9) + lint; skip python fallback when Ruff passed.
            if c == Checker::Ruff {
                break;
            }
        }
        ran.then(Vec::new)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

/// Mensagem sem números: o mesmo erro deslocado de linha é o mesmo erro.
fn key(d: &Diag) -> String {
    let mut k = String::with_capacity(d.msg.len());
    for ch in d.msg.chars() {
        if !ch.is_ascii_digit() {
            k.push(ch);
        } else if !k.ends_with('#') {
            k.push('#');
        }
    }
    k
}

/// `after` menos `before`, contando repetições (dois `x` indefinidos onde
/// havia um = um novo).
fn new_errors(before: &[Diag], after: Vec<Diag>) -> Vec<Diag> {
    let mut pool: Vec<String> = before.iter().map(key).collect();
    after
        .into_iter()
        .filter(|d| match pool.iter().position(|k| *k == key(d)) {
            Some(i) => {
                pool.swap_remove(i);
                false
            }
            None => true,
        })
        .collect()
}

fn render(errs: &[Diag]) -> String {
    let mut lines: Vec<String> = errs
        .iter()
        .take(MAX_ERRORS)
        .map(|d| format!("{PREFIX} linha {}: {}", d.line, d.msg))
        .collect();
    if errs.len() > MAX_ERRORS {
        lines.push(format!("{PREFIX} ... e mais {}", errs.len() - MAX_ERRORS));
    }
    lines.join("\n")
}

/// O conteúdo do arquivo antes da gravação (`None` dentro: não existia).
pub(crate) struct Before(Option<Vec<u8>>);

type FileStamp = Option<(u64, Option<SystemTime>)>;
type SettingsKey = (PathBuf, PathBuf);
type SettingsStamp = (FileStamp, FileStamp, FileStamp);
type EnabledSettingsCache = Mutex<HashMap<SettingsKey, (SettingsStamp, bool)>>;

fn file_stamp(path: &Path) -> FileStamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

fn post_write_check_enabled(workspace: &Path) -> bool {
    static CACHE: OnceLock<EnabledSettingsCache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let paths = zerith_core::Paths::resolve();
    let user_file = paths.settings();
    let key = (user_file.clone(), workspace.to_path_buf());
    let stamp = (
        file_stamp(&user_file),
        file_stamp(&workspace.join(".zerith").join("settings.toml")),
        file_stamp(&workspace.join(".zerith").join("settings.local.toml")),
    );
    if let Ok(g) = cache.lock() {
        if let Some((cached_stamp, enabled)) = g.get(&key) {
            if *cached_stamp == stamp {
                return *enabled;
            }
        }
    }
    let enabled = zerith_core::Settings::load(&paths, Some(workspace))
        .settings
        .context
        .post_write_check;
    if let Ok(mut g) = cache.lock() {
        g.insert(key, (stamp, enabled));
    }
    enabled
}

/// Chamar antes de gravar. `None` quando não há verificador para a
/// extensão ou `[context] post_write_check = false`.
pub(crate) fn before(workspace: &Path, path: &Path) -> Option<Before> {
    if checkers(path).is_empty() {
        return None;
    }
    if !post_write_check_enabled(workspace) {
        return None;
    }
    Some(Before(std::fs::read(path).ok()))
}

/// Chamar depois de gravar: as linhas dos erros novos, ou vazio. Nunca
/// falha: verificador ausente, lento ou quebrado vira silêncio.
pub(crate) fn after(path: &Path, before: Option<Before>) -> String {
    let Some(Before(old)) = before else {
        return String::new();
    };
    let Ok(new) = std::fs::read(path) else {
        return String::new();
    };
    let deadline = Instant::now() + BUDGET;
    let post = match diags(path, &new, deadline) {
        Some(p) if !p.is_empty() => p,
        _ => return String::new(),
    };
    // Sem "antes" (arquivo novo, ou não deu para checar): tudo é novo.
    let pre = old
        .and_then(|o| diags(path, &o, deadline))
        .unwrap_or_default();
    render(&new_errors(&pre, post))
}

/// `after`, já com a linha em branco antes, para colar no resultado.
pub(crate) fn append(msg: &mut String, path: &Path, before: Option<Before>) {
    let block = after(path, before);
    if !block.is_empty() {
        msg.push_str("\n\n");
        msg.push_str(&block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(line: u32, msg: &str) -> Diag {
        Diag {
            line,
            msg: msg.into(),
        }
    }

    #[test]
    fn parses_each_checker_output() {
        assert_eq!(
            Checker::Python.parse("2: IndentationError: expected an indented block after function definition on line 1\n"),
            vec![d(2, "IndentationError: expected an indented block after function definition on line 1")]
        );
        assert_eq!(
            Checker::Ruff.parse("C:\\t\\a.py:2:7: F821 Undefined name `x`\nFound 1 error.\n"),
            vec![d(2, "F821 Undefined name `x`")]
        );
        assert_eq!(
            Checker::Bash.parse("C:/t/a.sh: line 2: syntax error near unexpected token `fi'\nC:/t/a.sh: line 2: `fi'\n"),
            vec![d(2, "syntax error near unexpected token `fi'")]
        );
        assert_eq!(
            Checker::ShellCheck
                .parse("/t/a.sh:3:1: error: Couldn't parse this if expression. [SC1073]\n"),
            vec![d(3, "error: Couldn't parse this if expression. [SC1073]")]
        );
        assert_eq!(
            Checker::Node.parse("C:\\t\\a.js:1\nlet x = ;\n        ^\n\nSyntaxError: Unexpected token ';'\n    at wrapSafe (node:internal/modules/cjs/loader:1861:18)\n\nNode.js v24.21.0\n"),
            vec![d(1, "SyntaxError: Unexpected token ';'")]
        );
        assert_eq!(
            Checker::Pwsh.parse("1: Missing ')' in function parameter list.\r\n"),
            vec![d(1, "Missing ')' in function parameter list.")]
        );
        assert!(Checker::Node.parse("").is_empty());
        assert!(checkers(Path::new("a.rs")).is_empty());
        assert_eq!(
            checkers(Path::new("A.PY")),
            &[Checker::Ruff, Checker::Python]
        );
    }

    #[test]
    fn only_errors_the_edit_created_survive_and_at_most_twenty_are_shown() {
        let before = vec![
            d(3, "F821 Undefined name `x`"),
            d(9, "erro velho na linha 9"),
        ];
        let after = vec![
            d(5, "F821 Undefined name `x`"),
            d(12, "erro velho na linha 12"),
            d(6, "F821 Undefined name `x`"),
            d(7, "F821 Undefined name `y`"),
        ];
        let new = new_errors(&before, after);
        assert_eq!(
            new,
            vec![
                d(6, "F821 Undefined name `x`"),
                d(7, "F821 Undefined name `y`")
            ]
        );
        assert_eq!(
            render(&new[1..]),
            "[diagnóstico após gravar] linha 7: F821 Undefined name `y`"
        );
        let many: Vec<Diag> = (1..=25).map(|i| d(i, "e")).collect();
        let out = render(&many);
        assert_eq!(out.lines().count(), MAX_ERRORS + 1);
        assert!(out.ends_with("... e mais 5"), "{out}");
        assert!(render(&[]).is_empty());
    }

    /// Com o binário de verdade; pula onde ele não existe.
    #[test]
    fn real_checkers_report_only_the_new_error() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let dir = tempfile::Builder::new().tempdir_in(dir).unwrap();
        let cases = [
            (
                "a.py",
                "def f(:\n    pass\n",
                "x = 1\ndef f(:\n    pass\n",
                "def f():\n    return (\n",
            ),
            (
                "a.sh",
                "if true; then\necho\n",
                "x=1\nif true; then\necho\n",
                "echo ok\nfi\n",
            ),
        ];
        for (name, broken, shifted, other) in cases {
            let path = dir.path().join(name);
            if !checkers(&path).iter().any(|c| c.program().is_some()) {
                eprintln!("{name}: verificador ausente, pulando");
                continue;
            }
            let run = |old: Option<&str>, new: &str| {
                std::fs::write(&path, new).unwrap();
                after(&path, Some(Before(old.map(|o| o.as_bytes().to_vec()))))
            };
            // O mesmo erro, só deslocado: nada novo.
            assert_eq!(run(Some(broken), shifted), "", "{name}");
            // Arquivo novo quebrado: tudo é novo.
            let out = run(None, broken);
            assert!(
                out.starts_with("[diagnóstico após gravar] linha "),
                "{name}: {out}"
            );
            // Outro erro onde havia um: aparece.
            let out = run(Some(broken), other);
            assert!(out.contains("linha 2"), "{name}: {out}");
            // Arquivo limpo: silêncio.
            // Arquivo limpo em Python e em shell (`echo = 1` é SC2283 no shellcheck).
            assert_eq!(run(Some(broken), "# ok\n"), "", "{name}");
        }
    }
}
