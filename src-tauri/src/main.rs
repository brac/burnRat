// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `burnrat hook <Event>` is the lifecycle-hook subcommand Claude Code invokes
    // (see hookbridge.rs). It reads stdin, POSTs to the loopback bridge, and
    // exits — it must NOT spin up the Tauri app/window.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("hook") {
        let event = args.next().unwrap_or_default();
        std::process::exit(burnrat_lib::run_hook(&event));
    }
    burnrat_lib::run()
}
