// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // burnRat's hook subcommands, invoked by Claude Code (see hookbridge.rs).
    // They talk to the running app over the loopback bridge and exit — they must
    // NOT spin up the Tauri app/window.
    //   `burnrat hook <Event>` — fire-and-forget lifecycle edge → /state.
    //   `burnrat permission`   — blocking tool-permission request → /permission,
    //                            prints the decision to stdout for Claude.
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("hook") => {
            let event = args.next().unwrap_or_default();
            std::process::exit(burnrat_lib::run_hook(&event));
        }
        Some("permission") => std::process::exit(burnrat_lib::run_permission()),
        _ => {}
    }
    burnrat_lib::run()
}
