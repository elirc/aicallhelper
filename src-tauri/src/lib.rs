//! Tauri shell for AI Call Assistant v3 (§9, §11).
//!
//! The shell owns exactly the glue: IPC envelopes, event emission, window
//! lifecycle, hotkey, crash logging. Everything with behavior worth testing
//! against the spec lives in `app-core`.

mod commands;
mod events;
mod hotkey;
mod logging;
mod local_voice;
mod state;
mod window;

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::Manager;

use app_core::session::machine::SessionManager;
use app_core::store::{DesiredOsState, EffectReconciler, LaunchPlacement, SettingsStore};

use state::AppState;

/// RS-9: where do the pre-show milliseconds go? Printed to the dev terminal
/// only. Never into crash.log — that file is panic-only by design (§11), and
/// a timing line per launch would train readers to skim it.
#[cfg(debug_assertions)]
fn stage(t0: Instant, name: &str) {
    eprintln!("[startup] {name}: {:?}", t0.elapsed());
}

#[cfg(not(debug_assertions))]
fn stage(_t0: Instant, _name: &str) {}

pub fn run() {
    tauri::Builder::default()
        // Single-instance MUST be the first plugin: it decides whether this
        // process gets to live at all, before any other plugin allocates
        // anything a doomed process would leak.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window(window::MAIN_WINDOW) {
                // Unminimize BEFORE focusing — focusing a minimized window is
                // a no-op on Windows, which is exactly the "I clicked the icon
                // and nothing happened" bug this plugin exists to prevent.
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let t0 = Instant::now();
            let handle = app.handle().clone();

            let data_dir = handle.path().app_data_dir()?;
            // Best-effort: the store and the crash log both handle a missing
            // dir themselves; failing launch over a mkdir race helps no one.
            let _ = std::fs::create_dir_all(&data_dir);
            logging::install_panic_hook(data_dir.join("crash.log"));

            let store = SettingsStore::load_from(&data_dir);
            let startup = store.get();
            let saved_bounds = store.window_bounds();
            stage(t0, "settings loaded");

            // Registered before the state exists so the reconciler can be
            // seeded with what the OS actually accepted (a refused combo is
            // retried by the first save instead of counted as applied).
            let status = hotkey::apply_hotkey(&handle, &startup.hotkey);
            let hotkey_took = commands::hotkey_took(&status);

            app.manage(AppState {
                settings: Mutex::new(store),
                sessions: Arc::new(SessionManager::new()),
                audio: Mutex::new(None),
                hotkey: Mutex::new(status),
                // Seeded with exactly what the lines below apply before the
                // window is shown, so the first save's effect step changes
                // only what that save changed.
                effects: EffectReconciler::seeded(&DesiredOsState::of(&startup), hotkey_took),
                bounds_saver: Arc::new(window::DebouncedSaver::new()),
                reload_limiter: Mutex::new(window::ReloadLimiter::new(window::RELOAD_MIN_GAP)),
            });

            // The window is built here rather than declared in tauri.conf.json
            // because `on_navigation` only exists on the window builder — and
            // the navigation policy (§9: the app never navigates; https
            // bounces to the browser, everything else is dropped) must be
            // attached before the first page ever loads.
            //
            // No builder `.center()`: positioning is owned by
            // `restore_geometry` below, which runs before `show()`, so a
            // builder-time centre was one redundant SetWindowPos on a hidden
            // window (RS-9).
            let win = tauri::WebviewWindowBuilder::new(
                app,
                window::MAIN_WINDOW,
                tauri::WebviewUrl::default(),
            )
            .title("AI Call Assistant")
            .inner_size(460.0, 700.0)
            .min_inner_size(380.0, 520.0)
            .resizable(true)
            .maximizable(true)
            .decorations(true)
            // Matches the stylesheet background so the pre-CSS frame is not a
            // white flash on a dark app.
            .background_color(tauri::webview::Color(0x16, 0x18, 0x1d, 0xff))
            // Hidden until geometry + content protection are applied below, so
            // the first visible frame is already sized, positioned, protected.
            .visible(false)
            .on_navigation(window::handle_navigation)
            .build()?;
            stage(t0, "window built");

            window::restore_geometry(&win, saved_bounds);
            if startup.launch_placement == LaunchPlacement::Camera {
                // Size came from the saved bounds above; only the position is
                // replaced. Still pre-show, so the first visible frame is
                // already docked. Best-effort: a display that cannot be
                // found leaves the sanitizer's placement in force.
                let _ = window::dock_to_camera(&win, window::DockSize::Keep);
            }
            stage(t0, "geometry restored");
            window::install_crash_recovery(&win);

            // The moat feature: the app requests Windows capture exclusion,
            // always, with no toggle — and then VERIFIES that Windows applied
            // it, because `set_content_protected` cannot report a refusal
            // (tao discards SetWindowDisplayAffinity's result). If the read-
            // back is not WDA_EXCLUDEFROMCAPTURE, launching anyway would
            // silently break the promise, so setup fails and the panic hook
            // records the reason in crash.log. Whether a given conferencing
            // app honours the exclusion depends on its capture method and is
            // only known from recorded tests.
            win.set_content_protected(true)?;
            window::verify_capture_exclusion(&win)?;
            stage(t0, "content protected");

            let _ = win.set_always_on_top(startup.always_on_top);

            // The window is configured visible:false; everything above ran
            // off-screen, so the first paint the user sees is already sized,
            // positioned, and protected — no jump, no unprotected frame.
            let _ = win.show();
            let _ = win.set_focus();
            stage(t0, "shown");

            Ok(())
        })
        .on_window_event(window::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_settings,
            commands::start_session,
            commands::stop_session,
            commands::ask,
            commands::cancel_session,
            commands::session_outcome,
            commands::hotkey_status,
            commands::dock_to_camera,
            commands::local_prompt_budget,
            local_voice::local_voice_status,
            local_voice::prepare_local_voice,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run AI Call Assistant");
}
