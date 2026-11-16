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
