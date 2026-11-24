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
