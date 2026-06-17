mod blocks;
mod config;
mod data;
mod rate;
mod state;
mod userconfig;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::Serialize;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, Submenu},
    tray::TrayIconBuilder,
    Emitter, Manager,
};

use crate::config::Config;
use crate::data::DataMonitor;
use crate::rate::RateTracker;
use crate::state::StateMachine;
use crate::userconfig::UserConfig;

/// Runtime state the tray mutates and the poll loop reads.
struct Shared {
    /// Opacity as a percentage 0..=100.
    opacity_pct: AtomicU64,
    /// Whether the rat is currently click-through.
    click_through: AtomicBool,
    /// Where user overrides are persisted.
    config_path: std::path::PathBuf,
    /// The persisted user overrides.
    user: Mutex<UserConfig>,
}

impl Shared {
    fn persist(&self) {
        if let Ok(user) = self.user.lock() {
            user.save(&self.config_path);
        }
    }
}

/// Snapshot pushed to the frontend each poll. The view is dumb: it reads this
/// and redraws. No business logic lives in the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GameState {
    smoothed_tpm: f64,
    instant_tpm: f64,
    consumed: u64,
    consumed_with_cache: u64,
    projected: u64,
    time_remaining_min: i64,
    is_active: bool,
    opacity: f64,
    state: &'static str,
    event: Option<&'static str>,
}

fn apply_click_through(app: &tauri::AppHandle, on: bool) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_ignore_cursor_events(on);
    }
}

/// If the rat isn't fully on the primary monitor, recenter it there. This keeps
/// a remembered position that's already on the primary monitor, but rescues it
/// from a stale/off-screen position on a secondary monitor.
fn ensure_on_primary_monitor(app: &tauri::AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(Some(primary)), Ok(pos), Ok(size)) =
        (win.primary_monitor(), win.outer_position(), win.outer_size())
    else {
        return;
    };

    let mp = primary.position();
    let ms = primary.size();
    let (w, h) = (size.width as i32, size.height as i32);

    let within = pos.x >= mp.x
        && pos.y >= mp.y
        && pos.x + w <= mp.x + ms.width as i32
        && pos.y + h <= mp.y + ms.height as i32;

    if !within {
        let x = mp.x + (ms.width as i32 - w) / 2;
        let y = mp.y + (ms.height as i32 - h) / 2;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// Flip pass-through. The rat is interactive (grabbable/draggable) by default;
/// toggling pass-through ON makes clicks fall through to the app underneath.
fn toggle_click_through(app: &tauri::AppHandle, shared: &Shared) {
    let next = !shared.click_through.load(Ordering::Relaxed);
    shared.click_through.store(next, Ordering::Relaxed);
    apply_click_through(app, next);
}

/// Background loop: tail the JSONL, compute rate + blocks + state, emit.
fn spawn_poll_loop(app: tauri::AppHandle, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let config = Config::load();
        let interval = config.settings.poll_interval();
        let window_hours = config.settings.block_window_hours;

        let projects_dir = match DataMonitor::default_projects_dir() {
            Some(d) => d,
            None => {
                eprintln!("burnRat: could not resolve ~/.claude/projects");
                return;
            }
        };

        let idle_timeout = config.thresholds.idle_timeout_seconds;
        let activity_floor = config.thresholds.activity_floor_seconds;

        let mut monitor = DataMonitor::new(projects_dir.clone(), window_hours);
        let mut tracker = RateTracker::new(config.settings.rate_window_seconds);
        let mut machine = StateMachine::new(config.thresholds.clone());

        // Watch the projects tree: the moment Claude writes a token we react,
        // instead of busy-polling. `interval` is just the idle fallback tick so
        // time-based transitions (rate decay, idle->sleep) still fire.
        let (tx, rx) = std::sync::mpsc::channel();
        let _watcher = {
            use notify::{RecursiveMode, Watcher};
            notify::recommended_watcher(move |res| {
                let _ = tx.send(res);
            })
            .ok()
            .and_then(|mut w| {
                w.watch(&projects_dir, RecursiveMode::Recursive)
                    .ok()
                    .map(|_| w)
            })
        };

        loop {
            monitor.poll();
            let now = Utc::now();
            tracker.sample(now, monitor.cumulative_work);

            let smoothed = tracker.smoothed_tpm();
            let instant = tracker.instant_tpm();
            let opacity = shared.opacity_pct.load(Ordering::Relaxed) as f64 / 100.0;

            let grouped = blocks::group(&monitor.entries, window_hours, now);
            let active = blocks::active(&grouped);

            // "awake" = there's an active window AND tokens flowed recently. After
            // idle_timeout seconds without new tokens the rat naps (sleeping),
            // rather than waiting the full 5h for the window to lapse.
            let (consumed, consumed_with_cache, projected, remaining, awake, recent_activity) =
                match active {
                    Some(b) => {
                        let idle_secs = (now - b.actual_end).num_seconds();
                        (
                            b.work(),
                            b.total_with_cache(),
                            blocks::projected_work(b, smoothed, now),
                            blocks::time_remaining_min(b, now),
                            idle_secs <= idle_timeout,
                            idle_secs <= activity_floor,
                        )
                    }
                    None => (0, 0, 0, 0, false, false),
                };
            let is_active = awake;

            let (creature, event) =
                machine.update(awake, recent_activity, smoothed, instant, now);

            let game = GameState {
                smoothed_tpm: smoothed,
                instant_tpm: instant,
                consumed,
                consumed_with_cache,
                projected,
                time_remaining_min: remaining,
                is_active,
                opacity,
                state: creature.as_str(),
                event,
            };

            let _ = app.emit("game-state", &game);

            // Wait for the next filesystem event, or the idle tick — whichever
            // comes first. Drain any burst of events so we recompute once.
            if _watcher.is_some() {
                let _ = rx.recv_timeout(interval);
                while rx.try_recv().is_ok() {}
            } else {
                std::thread::sleep(interval);
            }
        }
    });
}

/// Build the tray icon and its menu (move mode, opacity, quit).
fn build_tray(app: &tauri::App, shared: Arc<Shared>) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Pass-Through  (Ctrl+Shift+M)", true, None::<&str>)?;

    // Opacity submenu — fixed steps.
    let current_pct = shared.opacity_pct.load(Ordering::Relaxed);
    let opacity_steps = [25u64, 50, 75, 100];
    let mut op_items: Vec<CheckMenuItem<_>> = Vec::new();
    for pct in opacity_steps {
        op_items.push(CheckMenuItem::with_id(
            app,
            format!("opacity:{pct}"),
            format!("{pct}%"),
            true,
            pct == current_pct,
            None::<&str>,
        )?);
    }
    let op_refs: Vec<&dyn tauri::menu::IsMenuItem<_>> =
        op_items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<_>).collect();
    let opacity_menu = Submenu::with_items(app, "Opacity", true, &op_refs)?;

    let quit = MenuItem::with_id(app, "quit", "Quit burnRat", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &opacity_menu, &quit])?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("burnRat")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id.as_ref();
            match id {
                "toggle" => toggle_click_through(app, &shared),
                "quit" => app.exit(0),
                _ if id.starts_with("opacity:") => {
                    if let Ok(pct) = id.trim_start_matches("opacity:").parse::<u64>() {
                        shared.opacity_pct.store(pct, Ordering::Relaxed);
                        if let Ok(mut u) = shared.user.lock() {
                            u.opacity = pct as f64 / 100.0;
                        }
                        shared.persist();
                    }
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // Persist only position — the rat is a fixed size.
                .with_state_flags(tauri_plugin_window_state::StateFlags::POSITION)
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(move |app| {
            let config = Config::load();

            // Resolve persisted user overrides (opacity).
            let config_path = app
                .path()
                .app_config_dir()
                .map(|d| d.join("settings.json"))
                .unwrap_or_else(|_| std::path::PathBuf::from("burnrat-settings.json"));
            let user = UserConfig::load(
                &config_path,
                config.settings.plan.clone(),
                config.settings.opacity,
            );

            let opacity_pct = (user.opacity * 100.0).round().clamp(0.0, 100.0) as u64;

            let shared = Arc::new(Shared {
                opacity_pct: AtomicU64::new(opacity_pct),
                click_through: AtomicBool::new(config.settings.click_through),
                config_path,
                user: Mutex::new(user),
            });

            // Start in the configured click-through mode.
            apply_click_through(app.handle(), config.settings.click_through);

            // Keep the rat on the primary monitor (the window-state plugin may
            // have restored an off-screen / wrong-monitor position).
            ensure_on_primary_monitor(app.handle());

            build_tray(app, shared.clone())?;

            // Global shortcut: Ctrl/Cmd+Shift+M toggles pass-through (so clicks
            // fall through to the app underneath when the rat is in the way).
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
                let sc = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyM);
                let sc_shared = shared.clone();
                if let Err(e) = app.global_shortcut().on_shortcut(sc, move |app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_click_through(app, &sc_shared);
                    }
                }) {
                    eprintln!("burnRat: failed to register move-mode shortcut: {e}");
                }
            }

            spawn_poll_loop(app.handle().clone(), shared);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
