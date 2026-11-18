//! The desktop binary. Everything it does lives in the library, so the same
//! code is testable without a window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    zerith_desktop::run()
}
