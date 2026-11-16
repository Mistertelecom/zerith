//! `zerith-ui` — opens the native window.
//!
//! `--tree` and `--snap <file.png>` are Rune's headless modes, so the same
//! binary proves the UI renders without a display.

use zerith_core::Paths;
use zerith_ui::{view, Shell};

fn main() {
    let spec = rune::AppSpec::new("Zerith", (1100.0, 720.0), || unreachable!(), view);
    let app = rune::App::new(spec);
    // The handle goes into the state so the turn worker can push events back
    // onto the UI thread. `run` opens the window and supplies the state.
    let me = app.handle();
    let paths = Paths::resolve();
    let _ = paths.ensure();
    let db = paths.db();
    app.init(move || Shell::open(db, me)).run();
}
