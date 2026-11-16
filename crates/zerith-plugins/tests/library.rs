use std::path::PathBuf;

use zerith_plugins::{Library, PluginError, Status};

fn scratch() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn plugin_dir(root: &std::path::Path, name: &str, manifest: &str) -> PathBuf {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("plugin.json"), manifest).unwrap();
    std::fs::write(d.join("README.md"), "demo").unwrap();
    d
}

const OK: &str =
    r#"{"name":"demo","version":"0.1.0","description":"demo","command":"node","args":["x.js"]}"#;

#[test]
fn installing_records_metadata_without_trusting_or_running_it() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    let src = plugin_dir(t.path(), "src", OK);
    let p = lib.install(&src).unwrap();
    assert_eq!(p.status, Status::Disabled);
    assert!(!p.trusted);
    assert!(p.dir.join("README.md").exists());
    assert_eq!(lib.plugins().unwrap().len(), 1);
}

#[test]
fn a_duplicate_name_is_rejected_instead_of_overwritten() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    let src = plugin_dir(t.path(), "src", OK);
    lib.install(&src).unwrap();
    let err = lib.install(&src).unwrap_err();
    assert!(matches!(err, PluginError::Exists(_)));
}

#[test]
fn launch_refuses_disabled_and_untrusted_plugins() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    let src = plugin_dir(t.path(), "src", OK);
    lib.install(&src).unwrap();
    // Disabled: nothing spawns.
    assert!(matches!(
        lib.launch("demo").err(),
        Some(PluginError::Disabled(_))
    ));
    lib.set_status("demo", Status::Enabled).unwrap();
    // Enabled but not reviewed: still nothing spawns.
    assert!(matches!(
        lib.launch("demo").err(),
        Some(PluginError::NotTrusted(_))
    ));
}

#[test]
fn a_broken_manifest_fails_the_install_and_leaves_the_library_usable() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    let bad = plugin_dir(t.path(), "bad", "{\"name\": }");
    assert!(lib.install(&bad).is_err());
    assert!(lib.plugins().unwrap().is_empty());
    let good = plugin_dir(t.path(), "good", OK);
    lib.install(&good).unwrap();
    assert_eq!(lib.plugins().unwrap().len(), 1);
}

#[test]
fn removing_takes_the_directory_with_it() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    let src = plugin_dir(t.path(), "src", OK);
    let p = lib.install(&src).unwrap();
    lib.remove("demo").unwrap();
    assert!(!p.dir.exists());
    assert!(lib.plugins().unwrap().is_empty());
}

fn bundle_dir(root: &std::path::Path) -> PathBuf {
    let d = plugin_dir(
        root,
        "bundle-src",
        r#"{"name":"pack","version":"1.0.0","skills":["skills/review"],"agents":["agents/rev.md"],
            "hooks":[{"event":"SessionStart","command":"echo oi"}],
            "mcp":[{"name":"docs","command":"docs-mcp","args":["--stdio"]}]}"#,
    );
    std::fs::create_dir_all(d.join("skills/review")).unwrap();
    std::fs::write(d.join("skills/review/SKILL.md"), "---\nname: review\n---\n").unwrap();
    std::fs::create_dir_all(d.join("agents")).unwrap();
    std::fs::write(d.join("agents/rev.md"), "revisa").unwrap();
    d
}

#[test]
fn a_bundle_contributes_nothing_until_enabled_and_reviewed() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    lib.install(&bundle_dir(t.path())).unwrap();
    assert!(lib.contributions().unwrap().skills.is_empty());
    lib.set_status("pack", Status::Enabled).unwrap();
    // Enabled but unreviewed is still nothing: hooks are shell commands.
    assert!(lib.contributions().unwrap().hooks.is_empty());
    lib.trust("pack").unwrap();
    let c = lib.contributions().unwrap();
    assert_eq!(c.skills.len(), 1);
    assert!(c.skills[0].1.join("SKILL.md").exists());
    assert_eq!(c.agents.len(), 1);
    assert_eq!(c.hooks[0].1.event, "SessionStart");
    assert_eq!(c.mcp[0].1.name, "pack-docs");
    // No `command`: nothing to launch, and it says why.
    assert!(matches!(
        lib.launch("pack").err(),
        Some(PluginError::Invalid(_))
    ));
    // Untrusting withdraws everything again.
    lib.untrust("pack").unwrap();
    assert!(lib.contributions().unwrap().mcp.is_empty());
}

#[test]
fn bundle_paths_cannot_escape_or_point_at_nothing() {
    let t = tempfile::Builder::new().tempdir_in(scratch()).unwrap();
    let lib = Library::open(&t.path().join("plugins")).unwrap();
    for (i, manifest) in [
        r#"{"name":"a","version":"1","skills":["../outside"]}"#,
        r#"{"name":"b","version":"1","agents":["missing.md"]}"#,
        r#"{"name":"c","version":"1","hooks":[{"event":"","command":"x"}]}"#,
        r#"{"name":"d","version":"1","mcp":[{"name":"s"}]}"#,
        r#"{"name":"e","version":"1","skills":["/abs"]}"#,
    ]
    .iter()
    .enumerate()
    {
        let src = plugin_dir(t.path(), &format!("bad{i}"), manifest);
        let err = lib.install(&src).unwrap_err();
        assert!(matches!(err, PluginError::Invalid(_)), "{manifest}: {err}");
    }
    assert!(lib.plugins().unwrap().is_empty());
}
