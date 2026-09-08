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

use tauri::Manager;

use app_core::session::machine::SessionManager;
use app_core::store::SettingsStore;

use state::{lock, AppState};

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
            let handle = app.handle().clone();

            let data_dir = handle.path().app_data_dir()?;
            // Best-effort: the store and the crash log both handle a missing
            // dir themselves; failing launch over a mkdir race helps no one.
            let _ = std::fs::create_dir_all(&data_dir);
            logging::install_panic_hook(data_dir.join("crash.log"));

            let store = SettingsStore::load_from(&data_dir);
            let startup = store.get().clone();
            let saved_bounds = store.window_bounds();

            app.manage(AppState {
                settings: Mutex::new(store),
                sessions: Arc::new(SessionManager::new()),
                audio: Mutex::new(None),
                hotkey: Mutex::new(hotkey::HotkeyState::disabled()),
                bounds_saver: Arc::new(window::DebouncedSaver::new()),
                reload_limiter: Mutex::new(window::ReloadLimiter::new(window::RELOAD_MIN_GAP)),
            });

            let status = hotkey::apply_hotkey(&handle, &startup.hotkey);
            *lock(&handle.state::<AppState>().hotkey) = status;

            // The window is built here rather than declared in tauri.conf.json
            // because `on_navigation` only exists on the window builder — and
            // the navigation policy (§9: the app never navigates; https
            // bounces to the browser, everything else is dropped) must be
            // attached before the first page ever loads.
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
            .center()
            // Hidden until geometry + content protection are applied below, so
            // the first visible frame is already sized, positioned, protected.
            .visible(false)
            .on_navigation(window::handle_navigation)
            .build()?;

            window::restore_geometry(&win, saved_bounds);
            window::install_crash_recovery(&win);

            // The moat feature: the window is invisible to screen sharing,
            // always, with no toggle. If the OS refuses, launching anyway
            // would silently break the one promise this app makes — so the
            // launch fails instead.
            win.set_content_protected(true)?;

            let _ = win.set_always_on_top(startup.always_on_top);

            // The window is configured visible:false; everything above ran
            // off-screen, so the first paint the user sees is already sized,
            // positioned, and protected — no jump, no unprotected frame.
            let _ = win.show();
            let _ = win.set_focus();

            Ok(())
        })
        .on_window_event(window::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            local_voice::local_voice_status,
            local_voice::prepare_local_voice,
            commands::set_settings,
            commands::start_session,
            commands::stop_session,
            commands::ask,
            commands::cancel_session,
            commands::hotkey_status,
            commands::open_external,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run AI Call Assistant");
}
