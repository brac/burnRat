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
use crate::data::{Awaiting, DataMonitor};
use crate::rate::{RateTracker, UnitSelector};
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
    /// Unit the frontend should render the rate readout in ("sec" / "min").
    rate_unit: &'static str,
    /// Model family driving the rat's hat ("opus"/"sonnet"/"haiku"/…/"none").
    model: &'static str,
}

/// Collapse a model id (e.g. "claude-opus-4-8") to a family the frontend maps
/// to a hat. Returns "none" when no model has been seen yet.
fn model_family(model: Option<&str>) -> &'static str {
    match model {
        Some(m) if m.contains("opus") => "opus",
        Some(m) if m.contains("sonnet") => "sonnet",
        Some(m) if m.contains("haiku") => "haiku",
        Some(m) if m.contains("fable") => "fable",
        Some(_) => "other",
        None => "none",
    }
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
        let done_hold = config.thresholds.done_hold_seconds;
        let sent_hold = config.thresholds.sent_hold_seconds;
        let cache_weight = config.settings.rate_cache_weight.max(0.0);

        // Self-calibrating usage ceiling for the approaching-limit warnings.
        // Rather than guess a per-plan cap, learn it from the largest COMPLETED
        // block in recent history (scanned once now). A manual planLimit (> 0)
        // overrides it; a ceiling below `limit_min_credible` is treated as
        // not-yet-enough-history and suppresses the warnings.
        let manual_limit = config.settings.plan_limit().unwrap_or(0);
        let limit_min_credible = config.settings.limit_min_credible_tokens;
        let history = chrono::Duration::days(config.settings.limit_history_days.max(0));
        let mut learned_peak =
            data::historical_peak_block(&projects_dir, window_hours, history, Utc::now());

        let long_running = config.thresholds.long_running_seconds;
        // Rate floor below which the rat is no longer "visibly working": the
        // working-state exit (down) cutoff. Used to keep the rat awake while the
        // burn rate is still elevated (see the nap gate in the loop).
        let working_floor = config.thresholds.states.working.down;

        let mut monitor = DataMonitor::new(projects_dir.clone(), window_hours);
        let mut tracker = RateTracker::new(config.settings.rate_window_seconds);
        let mut unit_selector = UnitSelector::new(config.settings.display);
        let mut machine = StateMachine::new(config.thresholds.clone());
        let mut refresh_tracker =
            state::RefreshTracker::new(config.thresholds.refreshed_hold_seconds);

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
            // Burn signal = work + cache·weight. Both counters are monotonic, so
            // the weighted sum is too (the rate tracker needs a monotonic input).
            let signal =
                monitor.cumulative_work as f64 + monitor.cumulative_cache as f64 * cache_weight;
            tracker.sample(now, signal.round() as u64);

            let smoothed = tracker.smoothed_tpm();
            let instant = tracker.instant_tpm();
            let opacity = shared.opacity_pct.load(Ordering::Relaxed) as f64 / 100.0;

            let grouped = blocks::group(&monitor.entries, window_hours, now);
            let active = blocks::active(&grouped);

            // "awake" = there's an active window AND tokens flowed recently. After
            // idle_timeout seconds without new tokens the rat naps (sleeping),
            // rather than waiting the full 5h for the window to lapse.
            // Smart napping: the nap clock runs from the last conversational line
            // (user OR assistant), so sending a message resets it — no jarring
            // done -> message -> nap. The activity floor (perk-up to working) still
            // runs from the last *token*, so a user message alone isn't "working".
            let nap_idle = monitor
                .last_activity()
                .map(|t| (now - t).num_seconds())
                .unwrap_or(i64::MAX);

            // What Claude is awaiting from the user. `asking` (an open question)
            // holds indefinitely; `done` (a finished turn) holds for done_hold
            // seconds, then is allowed to nap.
            let kind = if active.is_some() {
                monitor.awaiting()
            } else {
                Awaiting::None
            };
            let asking = matches!(kind, Awaiting::Asking);
            let done = matches!(kind, Awaiting::Done) && nap_idle <= done_hold;
            // An API error holds like a question (concern pose, doesn't nap).
            let error = matches!(kind, Awaiting::Error);
            let awaiting_user = done || asking || error;

            // A fresh user message (awaiting Claude) holds the idle pose longer
            // than a plain stall, so the rat doesn't nap through the dead air
            // before Claude starts responding.
            let sent = matches!(kind, Awaiting::Sent);
            let idle_hold = if sent { sent_hold } else { idle_timeout };

            let (consumed, consumed_with_cache, projected, remaining, base_awake, recent_activity) =
                match active {
                    Some(b) => {
                        let token_idle = (now - b.actual_end).num_seconds();
                        (
                            b.work(),
                            b.total_with_cache(),
                            blocks::projected_work(b, smoothed, now),
                            blocks::time_remaining_min(b, now),
                            // Awaiting the user holds; otherwise nap after the
                            // idle hold (longer right after a user message)
                            // measured from the last conversational line.
                            awaiting_user || nap_idle <= idle_hold,
                            token_idle <= activity_floor,
                        )
                    }
                    None => (0, 0, 0, 0, false, false),
                };

            // Quota refresh: the 5h window we were watching just rolled over.
            // Holds the `refreshed` pose (like waiting) even though the old
            // window went inactive, so fold it into "awake".
            let refreshed = refresh_tracker.update(
                blocks::latest_used_window_end(&grouped),
                recent_activity,
                now,
            );
            // Don't nap while the rat is still visibly burning. The nap clock
            // (nap_idle) is purely time-since-last-conversational-line, but that
            // line's timestamp can already be stale relative to wall-clock `now`
            // — e.g. a single long turn lands one line carrying a big token jump
            // (spiking the smoothed rate to stressed/onfire) whose timestamp
            // predates now by more than idle_timeout. Without this gate the rat
            // snaps straight from stressed/onfire to sleeping, skipping the
            // natural decay. Stay awake until the rate falls out of the working
            // band so it always glides down (stressed -> working -> calm -> sleep,
            // or the post-onfire spent crash) instead of cutting to a nap.
            let burning = smoothed >= working_floor;
            let awake = base_awake || refreshed || burning;
            let is_active = awake;

            // Long-running session: the active block has been going a long time.
            // TODO: first cut — only flags age; revisit whether it should be its
            // own sustained pose, factor in cumulative work, etc.
            let longrun = active
                .map(|b| (now - b.start).num_seconds() >= long_running)
                .unwrap_or(false);

            // Keep adapting the ceiling if a block completes while we're running.
            let mem_completed_peak = grouped
                .iter()
                .filter(|b| !b.is_active)
                .map(|b| b.total_with_cache())
                .max()
                .unwrap_or(0);
            learned_peak = learned_peak.max(mem_completed_peak);

            // Approaching-limit warning band: how close consumed (incl. cache) is
            // to the ceiling. 0 none, 1 within 10%, 2 within 5%, 3 within 1%
            // (most severe wins). Manual cap wins; else the learned ceiling, but
            // only once it's credible enough to not cry wolf.
            let ceiling = if manual_limit > 0 {
                manual_limit
            } else if learned_peak >= limit_min_credible {
                learned_peak
            } else {
                0
            };
            // 0 none, 1 within 10%, 2 within 5%, 3 within 1%, 4 at/over limit.
            let warn_level = if ceiling > 0 && active.is_some() {
                let remaining = 1.0 - consumed_with_cache as f64 / ceiling as f64;
                let a = &config.thresholds.approaching;
                if remaining <= 0.0 {
                    4
                } else if remaining <= a.warn1 {
                    3
                } else if remaining <= a.warn5 {
                    2
                } else if remaining <= a.warn10 {
                    1
                } else {
                    0
                }
            } else {
                0
            };

            let (base, event) =
                machine.update(awake, done, asking, recent_activity, smoothed, instant, now);

            // Final state, by priority: an API error wins; otherwise a pending
            // question/finished turn stays; otherwise a fresh quota refresh; then
            // the approaching-limit / at-limit warnings layer over the rate state;
            // and a long-running session shows only when otherwise idle.
            use state::CreatureState;
            let creature = if error && awake {
                CreatureState::Error
            } else if matches!(base, CreatureState::Waiting | CreatureState::Done) {
                base
            } else if refreshed {
                CreatureState::Refreshed
            } else {
                let warned = state::apply_approaching(base, warn_level);
                if longrun && warned == CreatureState::Calm {
                    CreatureState::LongRun
                } else {
                    warned
                }
            };
            let rate_unit = unit_selector.select(smoothed).as_str();
            let model = model_family(monitor.current_model());

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
                rate_unit,
                model,
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
