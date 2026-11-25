//! `zerith server install` / `uninstall`: serviço systemd `zerith-daemon`
//! (workflows + API) com usuário de sistema, pastas 0700, cofre em arquivo e
//! endurecimento. O plano é uma função pura ([`plan`]); `--dry-run` só imprime.
//!
//! Ideia (só conceito): instaladores de serviço do Goose/opencode; diretivas
//! de endurecimento na documentação do systemd (`systemd.exec(5)`).

use std::path::{Path, PathBuf};
use std::process::Command;

type Boxed = Box<dyn std::error::Error>;

pub const UNIT_NAME: &str = "zerith-daemon";
pub const UNIT_PATH: &str = "/etc/systemd/system/zerith-daemon.service";
pub const BIN_PATH: &str = "/usr/local/bin/zerith";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub user: String,
    pub home: String,
    pub bind: String,
    pub url: Option<String>,
    pub allow_public: bool,
    pub tz: String,
    pub dry_run: bool,
    pub force: bool,
}

/// O que já existe na máquina (coletado antes; o plano não olha o disco).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub user_exists: bool,
    /// O binário em execução já é `/usr/local/bin/zerith`.
    pub binary_in_place: bool,
    /// `<home>/server/config.toml` já existe (não roda `server init`).
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Run(Vec<String>),
    Write {
        path: String,
        content: String,
        mode: u32,
    },
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Run(argv) => write!(f, "{}", argv.join(" ")),
            Step::Write { path, mode, .. } => write!(f, "gravar {path} (modo {mode:o})"),
        }
    }
}

/// Recusa fora do Linux (systemd). `--dry-run` imprime em qualquer SO.
pub fn ensure_linux(os: &str, dry_run: bool) -> Result<(), Boxed> {
    if os == "linux" || dry_run {
        return Ok(());
    }
    Err(format!(
        "`zerith server install` instala um serviço systemd e só roda no Linux (este SO: {os}).\n\
         No Windows/macOS rode `zerith daemon` direto (ou como tarefa/serviço do sistema) e veja \
         docs/servidor.md; `--dry-run` mostra o que seria feito num Linux."
    )
    .into())
}

fn valid_user(u: &str) -> bool {
    let mut c = u.chars();
    u.len() <= 32
        && c.next().is_some_and(|f| f.is_ascii_lowercase() || f == '_')
        && c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

/// Valor que entra no unit file: sem espaço, aspas, `%`, `$` ou controle.
fn unit_safe(what: &str, v: &str) -> Result<(), Boxed> {
    if v.is_empty()
        || v.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '%' | '$' | '\\')
        })
    {
        return Err(format!("{what} inválido: '{v}'").into());
    }
    Ok(())
}

pub fn validate(o: &Opts) -> Result<(), Boxed> {
    if !valid_user(&o.user) {
        return Err(format!("--user inválido: '{}' (ex.: zerith)", o.user).into());
    }
    unit_safe("--home", &o.home)?;
    if !o.home.starts_with('/') || o.home == "/" || o.home.contains("..") {
        return Err(format!(
            "--home precisa ser caminho absoluto (ex.: /var/lib/zerith): '{}'",
            o.home
        )
        .into());
    }
    unit_safe("--tz", &o.tz)?;
    super::auth::check_bind(&o.bind, o.allow_public)?;
    if let Some(u) = &o.url {
        unit_safe("--url", u)?;
    }
    Ok(())
}

/// Conteúdo de `/etc/systemd/system/zerith-daemon.service`.
pub fn unit_file(o: &Opts) -> String {
    format!(
        "# Gerado por `zerith server install`. Remova com `zerith server uninstall`.
[Unit]
Description=Zerith daemon (workflows agendados + API do servidor)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={user}
Environment=ZERITH_HOME={home}
Environment=TZ={tz}
Environment=ZERITH_SECRETS_BACKEND=file
ExecStart={bin} daemon
Restart=always
RestartSec=5
UMask=0077
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths={home}
PrivateTmp=yes
ProtectHome=yes

[Install]
WantedBy=multi-user.target
",
        user = o.user,
        home = o.home,
        tz = o.tz,
        bin = BIN_PATH,
    )
}

fn argv(parts: &[&str]) -> Step {
    Step::Run(parts.iter().map(|s| s.to_string()).collect())
}

/// Passos da instalação, na ordem. `exe`: binário em execução.
pub fn plan(o: &Opts, f: &Facts, exe: &str) -> Vec<Step> {
    let mut s = Vec::new();
    if !f.user_exists {
        s.push(argv(&[
            "useradd",
            "--system",
            "--home",
            &o.home,
            "--shell",
            "/usr/sbin/nologin",
            &o.user,
        ]));
    }
    s.push(argv(&[
        "install", "-d", "-m", "0700", "-o", &o.user, "-g", &o.user, &o.home,
    ]));
    if !f.binary_in_place {
        s.push(argv(&["install", "-m", "0755", exe, BIN_PATH]));
    }
    s.push(Step::Write {
        path: UNIT_PATH.into(),
        content: unit_file(o),
        mode: 0o644,
    });
    if !f.configured {
        let home_env = format!("ZERITH_HOME={}", o.home);
        let mut init = vec![
            "runuser",
            "-u",
            &o.user,
            "--",
            "env",
            &home_env,
            "ZERITH_SECRETS_BACKEND=file",
            BIN_PATH,
            "server",
            "init",
            "--bind",
            &o.bind,
        ];
        if let Some(u) = &o.url {
            init.extend(["--url", u]);
        }
        if o.allow_public {
            init.push("--allow-public");
        }
        s.push(argv(&init));
    }
    s.push(argv(&["systemctl", "daemon-reload"]));
    s.push(argv(&["systemctl", "enable", "--now", UNIT_NAME]));
    s
}

/// Passos da desinstalação; `purge` apaga também usuário e dados.
pub fn uninstall_plan(user: &str, home: &str, purge: bool) -> Vec<Step> {
    let mut s = vec![
        argv(&["systemctl", "disable", "--now", UNIT_NAME]),
        argv(&["rm", "-f", UNIT_PATH]),
        argv(&["systemctl", "daemon-reload"]),
    ];
    if purge {
        s.push(argv(&["userdel", user]));
        s.push(argv(&["rm", "-rf", "--one-file-system", home]));
    }
    s
}

/// Porta de `bind` em uso segundo a saída de `ss -ltn`.
pub fn port_in_use(ss_output: &str, port: u16) -> bool {
    let suffix = format!(":{port}");
    ss_output
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(3))
        .any(|local| local.ends_with(&suffix))
}

fn output(cmd: &str, args: &[&str]) -> Option<String> {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run_steps(steps: &[Step]) -> Result<(), Boxed> {
    for st in steps {
        println!("+ {st}");
        match st {
            Step::Run(a) => {
                let status = Command::new(&a[0]).args(&a[1..]).status()?;
                if !status.success() {
                    return Err(format!("falhou ({status}): {st}").into());
                }
            }
            Step::Write {
                path,
                content,
                mode,
            } => {
                std::fs::write(path, content)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))?;
                }
                #[cfg(not(unix))]
                let _ = mode;
            }
        }
    }
    Ok(())
}

fn print_plan(steps: &[Step]) {
    for st in steps {
        println!("  {st}");
        if let Step::Write { content, .. } = st {
            for l in content.lines() {
                println!("      | {l}");
            }
        }
    }
}

fn require_root() -> Result<(), Boxed> {
    match output("id", &["-u"]) {
        Some(id) if id.trim() == "0" => Ok(()),
        _ => Err("precisa de root: rode com sudo".into()),
    }
}

pub fn install(o: &Opts) -> Result<(), Boxed> {
    ensure_linux(std::env::consts::OS, o.dry_run)?;
    validate(o)?;
    let linux = std::env::consts::OS == "linux";
    let exe = std::env::current_exe()?;
    let configured = Path::new(&o.home)
        .join("server")
        .join("config.toml")
        .is_file();
    let facts = Facts {
        user_exists: linux && output("id", &["-u", &o.user]).is_some(),
        binary_in_place: exe.canonicalize().ok() == PathBuf::from(BIN_PATH).canonicalize().ok(),
        configured,
    };
    let steps = plan(o, &facts, &exe.to_string_lossy());
    if o.dry_run {
        println!("plano (--dry-run, nada foi executado):");
        print_plan(&steps);
        if !linux {
            println!("(este SO não é Linux: a instalação de verdade só roda no Linux)");
        }
        return Ok(());
    }
    require_root()?;
    if !o.force && (Path::new(UNIT_PATH).exists() || configured) {
        return Err(format!(
            "já existe instalação ({UNIT_PATH} ou {}/server/config.toml); \
             use --force para regravar o serviço (os dados ficam), ou `zerith server uninstall`",
            o.home
        )
        .into());
    }
    let port = super::auth::check_bind(&o.bind, o.allow_public)?.port();
    let ss = output("ss", &["-ltn"]).ok_or("não consegui rodar `ss -ltn` (pacote iproute2)")?;
    if port_in_use(&ss, port) {
        // Reinstalando com --force, a porta pode ser a do próprio serviço.
        let ours = o.force
            && output("systemctl", &["is-active", UNIT_NAME]).is_some_and(|s| s.trim() == "active");
        if !ours {
            return Err(
                format!("porta {port} já está em uso (ss -ltn); escolha outra com --bind").into(),
            );
        }
    }
    run_steps(&steps)?;
    println!(
        "\nzerith-daemon instalado e rodando.\n\
         status:  systemctl status {UNIT_NAME}\n\
         logs:    journalctl -u {UNIT_NAME} -f\n\
         segredo: sudo -u {u} env ZERITH_HOME={h} ZERITH_SECRETS_BACKEND=file {BIN_PATH} secret set <nome>",
        u = o.user,
        h = o.home
    );
    if configured {
        println!(
            "(já estava configurado: novo código com `sudo -u {} env ZERITH_HOME={} {BIN_PATH} server token add <dispositivo>`)",
            o.user, o.home
        );
    }
    Ok(())
}

pub fn uninstall(user: &str, home: &str, purge: bool, dry_run: bool) -> Result<(), Boxed> {
    ensure_linux(std::env::consts::OS, dry_run)?;
    let steps = uninstall_plan(user, home, purge);
    if dry_run {
        println!("plano (--dry-run, nada foi executado):");
        print_plan(&steps);
        return Ok(());
    }
    require_root()?;
    run_steps(&steps)?;
    if purge {
        println!("serviço, usuário {user} e dados em {home} removidos ({BIN_PATH} ficou).");
    } else {
        println!("serviço removido; dados mantidos em {home} (apague com --purge).");
    }
    Ok(())
}

/// Lê as opções de `zerith server install ...`.
pub fn parse(args: &[String]) -> Result<Opts, Boxed> {
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let has = |name: &str| args.iter().any(|a| a == name);
    Ok(Opts {
        user: flag("--user").unwrap_or_else(|| "zerith".into()),
        home: flag("--home").unwrap_or_else(|| "/var/lib/zerith".into()),
        bind: flag("--bind").unwrap_or_else(|| super::DEFAULT_BIND.into()),
        url: flag("--url"),
        allow_public: has("--allow-public"),
        tz: flag("--tz").unwrap_or_else(|| "America/Sao_Paulo".into()),
        dry_run: has("--dry-run"),
        force: has("--force"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn unit_file_tem_ambiente_e_endurecimento() {
        let o = parse(&a("server install --home /srv/z --tz UTC --user noc")).unwrap();
        let u = unit_file(&o);
        for line in [
            "User=noc",
            "Group=noc",
            "Environment=ZERITH_HOME=/srv/z",
            "Environment=TZ=UTC",
            "Environment=ZERITH_SECRETS_BACKEND=file",
            "ExecStart=/usr/local/bin/zerith daemon",
            "Restart=always",
            "NoNewPrivileges=yes",
            "ProtectSystem=strict",
            "ReadWritePaths=/srv/z",
            "PrivateTmp=yes",
            "ProtectHome=yes",
            "WantedBy=multi-user.target",
        ] {
            assert!(u.lines().any(|l| l == line), "falta {line}\n{u}");
        }
    }

    #[test]
    fn plano_do_dry_run_na_ordem_e_pula_o_que_ja_existe() {
        let o = parse(&a("server install --bind 198.18.251.1:7979 --dry-run")).unwrap();
        assert!(o.dry_run);
        validate(&o).unwrap();
        let steps = plan(&o, &Facts::default(), "/tmp/zerith");
        let txt: Vec<String> = steps.iter().map(ToString::to_string).collect();
        assert_eq!(
            txt,
            [
                "useradd --system --home /var/lib/zerith --shell /usr/sbin/nologin zerith",
                "install -d -m 0700 -o zerith -g zerith /var/lib/zerith",
                "install -m 0755 /tmp/zerith /usr/local/bin/zerith",
                "gravar /etc/systemd/system/zerith-daemon.service (modo 644)",
                "runuser -u zerith -- env ZERITH_HOME=/var/lib/zerith ZERITH_SECRETS_BACKEND=file \
                 /usr/local/bin/zerith server init --bind 198.18.251.1:7979",
                "systemctl daemon-reload",
                "systemctl enable --now zerith-daemon",
            ]
        );
        let f = Facts {
            user_exists: true,
            binary_in_place: true,
            configured: true,
        };
        let txt: Vec<String> = plan(&o, &f, BIN_PATH)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(txt.len(), 4, "{txt:?}");
        assert!(!txt
            .iter()
            .any(|t| t.starts_with("useradd") || t.contains("server init")));

        assert_eq!(
            uninstall_plan("zerith", "/var/lib/zerith", false).len(),
            3,
            "sem --purge mantém dados"
        );
        let p: Vec<String> = uninstall_plan("zerith", "/var/lib/zerith", true)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(p.contains(&"userdel zerith".to_string()));
    }

    #[test]
    fn recusa_fora_do_linux_e_valores_perigosos() {
        assert!(ensure_linux("windows", false).is_err());
        assert!(ensure_linux("macos", false).is_err());
        assert!(ensure_linux("windows", true).is_ok(), "dry-run só imprime");
        assert!(ensure_linux("linux", false).is_ok());
        let bad = |s: &str| validate(&parse(&a(s)).unwrap()).is_err();
        assert!(
            bad("server install --bind 0.0.0.0:7878"),
            "público sem --allow-public"
        );
        assert!(bad("server install --user Root"));
        assert!(bad("server install --home relativo"));
        assert!(bad("server install --home /var/lib/z%h"));
        assert!(bad("server install --tz a\nb"));
        assert!(!bad("server install"));
    }

    #[test]
    fn porta_ocupada_pela_saida_do_ss() {
        let ss = "State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process\n\
                  LISTEN 0      4096   127.0.0.53%lo:53   0.0.0.0:*\n\
                  LISTEN 0      128    0.0.0.0:22         0.0.0.0:*\n\
                  LISTEN 0      128    [::]:7878          [::]:*\n";
        assert!(port_in_use(ss, 7878));
        assert!(port_in_use(ss, 22));
        assert!(!port_in_use(ss, 7979));
        assert!(!port_in_use(ss, 2), "não casa sufixo parcial");
    }
}
