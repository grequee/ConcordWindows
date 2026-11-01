#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = concord_app_lib::run() {
        eprintln!("Concord failed to start: {error:#}");
        std::process::exit(1);
    }
}
