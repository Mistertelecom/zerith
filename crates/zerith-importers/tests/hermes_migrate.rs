//! `hermes_migrate::run` contra um home do Hermes falso, numa pasta
//! temporária. Nunca toca os dados reais: home, raiz do Zerith e cofre são
//! todos de teste.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zerith_core::secrets::{SecretError, SecretStore};
use zerith_core::Paths;
use zerith_importers::hermes_migrate::{run, Options};
use zerith_importers::plan::{ImportPlan, Status};

/// Cofre em memória.
#[derive(Default)]
struct Vault(RefCell<HashMap<String, String>>);

impl SecretStore for Vault {
    fn get(&self, n: &str) -> Result<String, SecretError> {
        self.0
            .borrow()
            .get(n)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(n.into()))
    }
    fn set(&self, n: &str, v: &str) -> Result<(), SecretError> {
        self.0.borrow_mut().insert(n.into(), v.into());
        Ok(())
    }
    fn delete(&self, n: &str) -> Result<(), SecretError> {
        self.0.borrow_mut().remove(n);
        Ok(())
    }
}

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    paths: Paths,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn state_db(dir: &Path, rows: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("state.db")).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, model TEXT, started_at REAL,
            title TEXT, archived INTEGER, pinned INTEGER);
         CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT,
            role TEXT, content TEXT, timestamp REAL, active INTEGER);
         {rows}"
    ))
    .unwrap();
}

pub const SECRET_VALUE: &str = "Xy9#comun1ty";

fn fixture() -> Fx {
    let t = tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let home = t.path().join("hermes");
    state_db(
        &home,
        "INSERT INTO sessions VALUES
            ('a', 'm', 1700000000.0, 'Fixada', 0, 1),
            ('b', 'm', 1700000100.0, 'Comum', 0, 0),
            ('c', 'm', 1700000200.0, 'Velha', 1, 0);
         INSERT INTO messages(session_id, role, content, timestamp, active) VALUES
            ('a', 'user', 'oi', 1700000001.0, 1),
            ('b', 'user', 'olá', 1700000101.0, 1);",
    );
    state_db(
        &home.join("profiles/netops"),
        "INSERT INTO sessions VALUES ('x', 'm', 1700000000.0, 'Do perfil', 0, 0);",
    );
    write(
        &home.join("memories/MEMORY.md"),
        &format!(
            "Cliente Acme usa NE20 na borda.\n§\nSwitch core da Acme: SNMPv2c {SECRET_VALUE}=WRITE em 10.0.0.1.\n§\nCliente Acme usa NE20 na borda.\n§\nchave solta sk-abcdefghijklmnopqrstuvwxyz123456\n"
        ),
    );
    write(
        &home.join("memories/USER.md"),
        "Sempre responder em português.\n§\nGosta de respostas curtas.\n",
    );
    write(
        &home.join("SOUL.md"),
        "Você é o Hermes, direto e curto. Config em ~/.hermes/config.yaml.\n",
    );
    write(
        &home.join("AGENTS.md"),
        "# Everything Claude Code (ECC) — Agent Instructions\n| a | b |\n",
    );
    write(
        &home.join("creds/acme-borda.txt"),
        &format!(
            "# NE20 da borda, cuidado\nhost: 10.0.0.1\nport: 2222\nlogin: admin\npassword: {CRED_PW}\nprotocolo: ssh (ssh-rsa)\nplataforma: Huawei NE20\nobservacao: produção, acesso alternativo (suporte/{ALT_PW})\n"
        ),
    );
    write(
        &home.join("creds/acme-olt-parks.txt"),
        &format!("host: 10.0.0.9\nlogin: a\npassword: {CRED_PW}2\ncli_login: root\ncli_password: {CRED_PW}3\nprotocolo: telnet\n"),
    );
    write(
        &home.join("creds/acme-segredos.txt"),
        &format!("# lista\nwifi: {CRED_PW}4\n{CRED_PW}5\n"),
    );
    let paths = Paths::with_root(t.path().join("zerith"));
    Fx { _t: t, home, paths }
}

fn opts(dry_run: bool) -> Options {
    Options {
        dry_run,
        ..Default::default()
    }
}

fn count(plan: &ImportPlan, kind: &str, status: Status) -> usize {
    plan.items
        .iter()
        .filter(|i| i.kind == kind && i.status == status)
        .count()
}

const CRED_PW: &str = "Pw!secr3t";
const ALT_PW: &str = "Alt#9pass";

#[test]
fn credentials_become_inventory_without_passwords() {
    let fx = fixture();
    let vault = Vault::default();
    let plan = run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    let inv = std::fs::read_to_string(fx.paths.root().join("inventory.toml")).unwrap();
    assert!(!inv.contains(CRED_PW) && !inv.contains(ALT_PW), "{inv}");
    assert!(!rendered(&plan).contains(CRED_PW));
    // Sem --migrate-secrets nenhuma senha de creds/ vai ao cofre (o valor
    // tirado da memória vai sempre: é o único jeito de ela entrar).
    assert!(
        vault
            .0
            .borrow()
            .keys()
            .all(|k| k.starts_with("hermes-memoria-")),
        "sem --migrate-secrets nada de creds/ vai ao cofre"
    );
    assert!(plan
        .stripped_secrets
        .contains(&"credencial:acme-borda".to_string()));
    let devs = zerith_tools::network::parse_inventory(&inv).unwrap();
    assert_eq!(devs.len(), 2);
    let b = devs.iter().find(|d| d.name == "borda").unwrap();
    assert_eq!(b.client.as_deref(), Some("acme"));
    assert_eq!(
        (b.host.as_str(), b.port, b.user.as_str()),
        ("10.0.0.1", 2222, "admin")
    );
    assert_eq!(b.vendor, zerith_tools::network::Vendor::HuaweiVrp);
    assert_eq!(b.role, zerith_tools::network::DeviceRole::Border);
    assert!(b.legacy_ssh);
    assert!(b.tags.contains(&"producao".to_string()));
    assert_eq!(b.secret_ref.as_deref(), Some("acme-borda"));
    let d = b.description.as_deref().unwrap();
    assert!(
        d.contains("NE20 da borda") && d.contains("senha no cofre: acme-borda-alt1"),
        "{d}"
    );
    let o = devs.iter().find(|d| d.name == "olt-parks").unwrap();
    assert_eq!(o.transport, zerith_tools::network::DeviceTransport::Telnet);
    assert_eq!(o.port, 23);
    assert!(o.tags.contains(&"parks".to_string()) && o.tags.contains(&"olt".to_string()));
    assert_eq!(
        count(&plan, "credencial", Status::Skipped),
        1,
        "lista sem host e sem flag"
    );

    // Agora com a flag: as senhas vão ao cofre, o inventário não muda.
    let o2 = Options {
        dry_run: false,
        migrate_secrets: true,
        ..Default::default()
    };
    run(&fx.home, &fx.paths, &o2, &vault).unwrap();
    let v = vault.0.borrow();
    assert_eq!(v["acme-borda"], CRED_PW);
    assert_eq!(v["acme-borda-alt1"], ALT_PW);
    assert_eq!(v["acme-olt-parks-cli"], format!("{CRED_PW}3"));
    assert_eq!(v["acme-segredos"], format!("wifi: {CRED_PW}4 | {CRED_PW}5"));
}

/// Tudo que um plano/relatório mostra, para procurar vazamento de valores.
fn rendered(plan: &ImportPlan) -> String {
    format!(
        "{}{}",
        serde_json::to_string(plan).unwrap(),
        plan.markdown()
    )
}

#[test]
fn the_dry_run_writes_nothing() {
    let fx = fixture();
    let vault = Vault::default();
    let plan = run(&fx.home, &fx.paths, &opts(true), &vault).unwrap();
    assert!(!fx.paths.root().exists(), "dry-run criou a raiz do Zerith");
    assert!(vault.0.borrow().is_empty());
    assert_eq!(count(&plan, "sessao", Status::Imported), 2);
    assert!(count(&plan, "memoria", Status::Imported) >= 3);
    assert!(!rendered(&plan).contains(SECRET_VALUE));
}

#[test]
fn sessions_keep_pinned_and_archived_and_profiles() {
    let fx = fixture();
    let vault = Vault::default();
    run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    let store = zerith_sessions::Store::open(fx.paths.db()).unwrap();
    assert_eq!(store.pinned_sessions().unwrap(), ["hermes-default-a"]);
    let s = |id: &str| store.session(id).unwrap().unwrap();
    assert!(s("hermes-default-c").archived);
    assert!(!s("hermes-default-b").archived);
    assert!(store.session("hermes-netops-x").unwrap().is_some());
}

#[test]
fn memory_values_go_to_the_vault_and_the_rest_to_overflow() {
    let fx = fixture();
    let vault = Vault::default();
    let plan = run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    assert!(!rendered(&plan).contains(SECRET_VALUE));
    let v = vault.0.borrow();
    let (name, value) = v.iter().next().expect("valor no cofre");
    assert_eq!(value, SECRET_VALUE);
    assert!(name.starts_with("hermes-memoria-"));

    let conn = rusqlite::Connection::open(fx.paths.db()).unwrap();
    let facts: Vec<(String, String)> = conn
        .prepare("SELECT kind, text FROM memory_facts WHERE deleted = 0 ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let all: String = facts.iter().map(|f| f.1.clone()).collect();
    assert!(!all.contains(SECRET_VALUE));
    assert!(all.contains(&format!("valor no cofre ({name})")));
    // Duplicada entrou uma vez; a chave solta foi para o overflow.
    assert_eq!(facts.iter().filter(|f| f.1.contains("NE20")).count(), 1);
    assert!(!all.contains("sk-abc"));
    assert!(facts.contains(&(
        "instruction".into(),
        "Sempre responder em português.".into()
    )));
    assert!(facts.contains(&("preference".into(), "Gosta de respostas curtas.".into())));
    assert!(facts.iter().any(|f| f.0 == "entity"));
    let dir = PathBuf::from(plan.report_dir.as_ref().unwrap());
    let over = std::fs::read_to_string(dir.join("overflow/memoria-default-MEMORY.md")).unwrap();
    assert!(over.contains("sk-abc"));
    assert!(dir.join("report.json").is_file() && dir.join("summary.md").is_file());
    assert!(PathBuf::from(plan.backup_dir.unwrap()).is_dir());
}

#[test]
fn persona_is_appended_rebranded_and_the_generic_guide_archived() {
    let fx = fixture();
    std::fs::create_dir_all(fx.paths.root()).unwrap();
    std::fs::write(fx.paths.instructions(), "# Minhas regras\n\nNão apagar.\n").unwrap();
    let vault = Vault::default();
    let plan = run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    let agents = std::fs::read_to_string(fx.paths.instructions()).unwrap();
    assert!(agents.starts_with("# Minhas regras\n\nNão apagar.\n"));
    assert!(agents.contains("Você é o Zerith, direto e curto."));
    assert!(agents.contains("~/.hermes/config.yaml"));
    assert!(!agents.contains("ECC"));
    assert_eq!(count(&plan, "persona", Status::Archived), 1);
    let dir = PathBuf::from(plan.report_dir.unwrap());
    assert!(dir.join("archive/persona/AGENTS.md").is_file());
    // Editada no Zerith: a próxima rodada vê conflito, não sobrescreve.
    std::fs::write(fx.paths.instructions(), agents.replace("direto", "seco")).unwrap();
    std::fs::write(fx.home.join("SOUL.md"), "Você é o Hermes, outro.\n").unwrap();
    let again = run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    assert_eq!(count(&again, "persona", Status::Conflict), 1);
    assert!(std::fs::read_to_string(fx.paths.instructions())
        .unwrap()
        .contains("seco"));
}

#[test]
fn a_second_run_skips_everything() {
    let fx = fixture();
    let vault = Vault::default();
    run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    let again = run(&fx.home, &fx.paths, &opts(false), &vault).unwrap();
    let not_skipped: Vec<_> = again
        .items
        .iter()
        .filter(|i| i.status != Status::Skipped)
        .collect();
    assert!(not_skipped.is_empty(), "{not_skipped:#?}");
    let plan = run(&fx.home, &fx.paths, &opts(true), &vault).unwrap();
    assert!(plan.items.iter().all(|i| i.status == Status::Skipped));
}
