//! Server configuration layers and the project-approval rule.

use zerith_core::Paths;
use zerith_mcp::config::{add_server, approve_server, load_servers, parse_add, remove_server};
use zerith_mcp::Origin;

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

#[test]
fn a_project_cannot_approve_its_own_server() {
    let t = scratch();
    let paths = Paths::with_root(t.path().join("home"));
    let project = t.path().join("proj");
    std::fs::create_dir_all(project.join(".zerith")).unwrap();
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(
        paths.settings(),
        "[[mcp.servers]]\nname = \"fs\"\ncommand = \"npx\"\nargs = [\"-y\", \"fs\"]\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".zerith").join("settings.toml"),
        "[mcp]\napproved = [\"evil\"]\n\n[[mcp.servers]]\nname = \"evil\"\ncommand = \"calc\"\n",
    )
    .unwrap();

    let l = load_servers(&paths, Some(&project));
    assert_eq!(l.servers.len(), 1);
    assert_eq!(l.servers[0].name, "fs");
    assert_eq!(l.servers[0].origin, Origin::User);
    assert_eq!(l.unapproved.len(), 1);
    assert_eq!(l.unapproved[0].name, "evil");
}

#[test]
fn approval_from_the_user_file_lets_the_project_server_run() {
    let t = scratch();
    let paths = Paths::with_root(t.path().join("home"));
    let project = t.path().join("proj");
    std::fs::create_dir_all(project.join(".zerith")).unwrap();
    std::fs::write(
        project.join(".zerith").join("settings.toml"),
        "[[mcp.servers]]\nname = \"evil\"\ncommand = \"calc\"\n",
    )
    .unwrap();

    let l = load_servers(&paths, Some(&project));
    assert!(l.servers.is_empty());
    assert_eq!(l.unapproved.len(), 1);

    approve_server(&paths, "evil").unwrap();
    let l = load_servers(&paths, Some(&project));
    assert_eq!(l.servers.len(), 1);
    assert_eq!(l.servers[0].origin, Origin::Project);
    assert!(l.unapproved.is_empty());
}

#[test]
fn add_and_remove_round_trip_through_mcp_toml() {
    let t = scratch();
    let paths = Paths::with_root(t.path().join("home"));
    let s = parse_add(&["gh".into(), "--".into(), "gh-mcp".into(), "--stdio".into()]).unwrap();
    add_server(&paths, s).unwrap();
    let l = load_servers(&paths, None);
    assert_eq!(l.servers[0].args, ["--stdio"]);
    // Adding again replaces, never duplicates.
    let other = parse_add(&["gh".into(), "--".into(), "other".into()]).unwrap();
    add_server(&paths, other).unwrap();
    let l = load_servers(&paths, None);
    assert_eq!(l.servers.len(), 1);
    assert_eq!(l.servers[0].command.as_deref(), Some("other"));
    assert!(remove_server(&paths, "gh").unwrap());
    assert!(!remove_server(&paths, "gh").unwrap());
    assert!(load_servers(&paths, None).servers.is_empty());
}

#[test]
fn an_invalid_file_is_reported_and_the_rest_still_loads() {
    let t = scratch();
    let paths = Paths::with_root(t.path().join("home"));
    std::fs::create_dir_all(paths.root()).unwrap();
    std::fs::write(paths.root().join("mcp.toml"), "[[mcp.servers\nname=").unwrap();
    std::fs::write(
        paths.settings(),
        "[[mcp.servers]]\nname = \"ok\"\nurl = \"https://x.test/mcp\"\n\n[[mcp.servers]]\nname = \"both\"\ncommand = \"a\"\nurl = \"http://b\"\n",
    )
    .unwrap();
    let l = load_servers(&paths, None);
    assert_eq!(l.servers.len(), 1);
    assert_eq!(l.errors.len(), 2, "{:?}", l.errors);
}
