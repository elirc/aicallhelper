//! Global shortcut registration (§9).

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// What `hotkey_status` reports. `registered: false` alongside a non-empty
/// accelerator is the honest "another app owns this combo (or it did not
/// parse)" signal — the UI can tell the user instead of presenting a key that
/// silently does nothing.
#[derive(Debug, Clone, Serialize)]
pub struct HotkeyState {
    pub accelerator: String,
    pub registered: bool,
}

impl HotkeyState {
    pub fn disabled() -> Self {
        Self { accelerator: String::new(), registered: false }
    }
}

/// (Re)register the toggle hotkey to match `accelerator`, returning what
/// actually took effect. Called at startup and again whenever the setting
/// changes.
pub fn apply_hotkey(app: &AppHandle, accelerator: &str) -> HotkeyState {
    let shortcuts = app.global_shortcut();

    // Always clear first. After a settings change, a surviving registration
    // would be a ghost key: it fires toggle from a combo the user believes
    // they removed. This app owns exactly one shortcut, so clearing all is
    // clearing ours.
    let _ = shortcuts.unregister_all();

    let accelerator = accelerator.trim();
    if accelerator.is_empty() {
        // Empty means the user disabled the shortcut. Registering nothing and
        // reporting registered:false is the requested state, not a failure —
        // and it must never spring back to the default.
        return HotkeyState::disabled();
    }

    let registered = shortcuts
        .on_shortcut(accelerator, |app, _shortcut, event| {
            // Press only: reacting to press AND release would toggle twice per
            // tap, turning every recording into an instant stop.
            if event.state() == ShortcutState::Pressed {
                let _ = app.emit("hotkey:toggle", ());
            }
        })
        .is_ok();

    // Parse failures and OS-level rejection (combo owned by another app) both
    // land here as registered:false — recorded rather than swallowed, so
    // `hotkey_status` never advertises a dead key.
    HotkeyState { accelerator: accelerator.to_string(), registered }
}
