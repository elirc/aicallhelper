//! Reconciling operating-system effects with the committed settings (R5,
//! ADR 016).
//!
//! Three settings reach outside the process: the global hotkey registration,
//! the always-on-top window flag, and the dock-under-the-camera placement.
//! The old shell applied each one from the before/after pair of ONE patch,
//! after the store lock was released. Two saves could therefore commit A then
//! B and run their effects B then A, leaving the OS with A's hotkey while the
//! disk held B's. Skipping "older" effect runs does not fix it either: if A
//! changed the hotkey and B only changed the style, skipping A leaves the old
//! hotkey registered forever.
//!
//! The rule here is instead: after every commit, apply the CURRENT committed
//! settings' OS state. Runs are serialized by one lock, each run reads the
//! desired state inside that lock, and each effect is compared against what
//! was last applied rather than against the patch. Whatever order commits
//! and runs interleave in, the run that happens last applies the newest
//! committed state, and every earlier run is idempotent work toward it.

use std::sync::{Mutex, MutexGuard};

use super::{LaunchPlacement, Settings};

/// The part of `Settings` that the OS must reflect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredOsState {
    /// Trimmed accelerator; "" means no shortcut.
    pub hotkey: String,
    pub always_on_top: bool,
    pub launch_placement: LaunchPlacement,
}

impl DesiredOsState {
    pub fn of(settings: &Settings) -> Self {
        Self {
            hotkey: settings.hotkey.trim().to_string(),
            always_on_top: settings.always_on_top,
            launch_placement: settings.launch_placement,
        }
    }
}

/// The OS calls the reconciler makes. The shell implements it over Tauri;
/// tests implement it with a recorder.
pub trait OsEffects {
    /// Replace the registered shortcut with exactly `accelerator` ("" = none)
    /// and record the outcome where `hotkey_status` reads it. Returns whether
    /// the OS now reflects `accelerator` (registered, or "" and cleared). A
    /// `false` is not remembered as applied, so the next run tries again: a
    /// combination another app has since released starts working on the next
    /// save instead of staying dead until a restart.
    fn apply_hotkey(&self, accelerator: &str) -> bool;
    fn set_always_on_top(&self, on: bool);
    /// Move the window under the camera at its current size. Only called when
    /// the placement newly becomes `Camera`: choosing it demonstrates it.
    fn dock_to_camera(&self);
}

/// What one reconcile run did, for tests and diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub hotkey: Option<String>,
    pub always_on_top: Option<bool>,
    pub docked: bool,
}

impl ReconcileReport {
    pub fn is_noop(&self) -> bool {
        self.hotkey.is_none() && self.always_on_top.is_none() && !self.docked
    }
}

/// What the OS was last told. Held for the whole of a run, so runs never
/// overlap and "last applied" is always exactly what the OS saw last.
#[derive(Debug, Default)]
struct Applied {
    hotkey: Option<String>,
    always_on_top: Option<bool>,
    launch_placement: Option<LaunchPlacement>,
}

#[derive(Debug, Default)]
pub struct EffectReconciler {
    applied: Mutex<Applied>,
}

impl EffectReconciler {
    /// A reconciler that knows nothing has been applied: its first run
    /// applies every effect.
    pub fn new() -> Self {
        Self::default()
    }

    /// A reconciler for state the caller already applied itself (startup
    /// registers the hotkey, sets the flag and docks before the window is
    /// shown), so the first post-save run changes only what the save changed.
    ///
    /// `hotkey_took` is what the startup registration returned: a hotkey the
    /// OS refused is seeded as not applied, so the first save retries it.
    pub fn seeded(applied: &DesiredOsState, hotkey_took: bool) -> Self {
        Self {
            applied: Mutex::new(Applied {
                hotkey: hotkey_took.then(|| applied.hotkey.clone()),
                always_on_top: Some(applied.always_on_top),
                launch_placement: Some(applied.launch_placement),
            }),
        }
    }

    /// Bring the OS in line with `current()`, which must read the COMMITTED
    /// settings. It is called inside the run lock, so a run that starts after
    /// a commit always sees that commit or a newer one.
    ///
    /// Blocking: the shell's hotkey call waits on the main thread. Call it
    /// from a blocking-pool thread, never from the event loop.
    pub fn reconcile(
        &self,
        current: impl FnOnce() -> DesiredOsState,
        os: &dyn OsEffects,
    ) -> ReconcileReport {
        let mut applied = self.lock();
        let desired = current();
        let mut report = ReconcileReport::default();

        if applied.hotkey.as_deref() != Some(desired.hotkey.as_str()) {
            let took = os.apply_hotkey(&desired.hotkey);
            // Only a registration that took is "applied"; a refused one stays
            // pending and is retried by the next run.
            applied.hotkey = took.then(|| desired.hotkey.clone());
            report.hotkey = Some(desired.hotkey.clone());
        }
        if applied.always_on_top != Some(desired.always_on_top) {
            os.set_always_on_top(desired.always_on_top);
            applied.always_on_top = Some(desired.always_on_top);
            report.always_on_top = Some(desired.always_on_top);
        }
        // Docking is an action, not a level: it fires on the transition into
        // Camera and never again while Camera stays chosen, so an unrelated
        // save does not yank a window the user has since moved.
        if desired.launch_placement == LaunchPlacement::Camera
            && applied.launch_placement != Some(LaunchPlacement::Camera)
        {
            os.dock_to_camera();
            report.docked = true;
        }
        applied.launch_placement = Some(desired.launch_placement);
        report
    }

    // Poison-tolerant: `Applied` is updated field by field right after each
    // OS call, so a panic inside an OS call leaves at worst that one effect
    // marked unapplied, and the next run retries it.
    fn lock(&self) -> MutexGuard<'_, Applied> {
        self.applied.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::llm::AnswerStyle;
    use crate::store::{CallProfilePatch, SettingsPatch, SettingsStore, SettingsView};
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    /// Records every OS call and the resulting OS state.
    #[derive(Default)]
    struct FakeOs {
        calls: Mutex<Vec<String>>,
        hotkey: Mutex<Option<String>>,
        on_top: Mutex<Option<bool>>,
        /// Accelerators another app owns: registering them fails.
        taken: Mutex<Vec<String>>,
    }

    impl FakeOs {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn state(&self) -> (Option<String>, Option<bool>) {
            (self.hotkey.lock().unwrap().clone(), *self.on_top.lock().unwrap())
        }
    }

    impl OsEffects for FakeOs {
        fn apply_hotkey(&self, accelerator: &str) -> bool {
            self.calls.lock().unwrap().push(format!("hotkey {accelerator}"));
            if self.taken.lock().unwrap().iter().any(|t| t == accelerator) {
                // Like the real plugin: the old key is cleared first, then
                // the new one is refused.
                *self.hotkey.lock().unwrap() = None;
                return false;
            }
            *self.hotkey.lock().unwrap() = Some(accelerator.to_string());
            true
        }
        fn set_always_on_top(&self, on: bool) {
            self.calls.lock().unwrap().push(format!("on_top {on}"));
            *self.on_top.lock().unwrap() = Some(on);
        }
        fn dock_to_camera(&self) {
            self.calls.lock().unwrap().push("dock".into());
        }
    }

    fn desired(hotkey: &str, on_top: bool, placement: LaunchPlacement) -> DesiredOsState {
        DesiredOsState { hotkey: hotkey.into(), always_on_top: on_top, launch_placement: placement }
    }

    fn hotkey(v: &str) -> SettingsPatch {
        SettingsPatch { hotkey: Some(v.into()), ..Default::default() }
    }

    fn style(v: AnswerStyle) -> SettingsPatch {
        SettingsPatch { answer_style: Some(v), ..Default::default() }
    }

    /// Disk, store and OS must describe the same state.
    fn assert_agree(dir: &std::path::Path, store: &SettingsStore, os: &FakeOs) {
        let disk = SettingsStore::load_from(dir).get();
        assert_eq!(disk, store.get(), "memory matches disk");
        let (hk, top) = os.state();
        assert_eq!(hk.as_deref(), Some(disk.hotkey.as_str()), "registered hotkey matches disk");
        assert_eq!(top, Some(disk.always_on_top), "window flag matches disk");
    }

    fn seeded_for(store: &SettingsStore) -> EffectReconciler {
        EffectReconciler::seeded(&DesiredOsState::of(&store.get()), true)
    }

    #[test]
    fn a_fresh_reconciler_applies_everything_once_then_nothing() {
        let r = EffectReconciler::new();
        let os = FakeOs::default();
        let first = r.reconcile(|| desired("Alt+Q", true, LaunchPlacement::Camera), &os);
        assert_eq!(os.calls(), ["hotkey Alt+Q", "on_top true", "dock"]);
        assert!(!first.is_noop());
        let second = r.reconcile(|| desired("Alt+Q", true, LaunchPlacement::Camera), &os);
        assert!(second.is_noop(), "idempotent: the same desired state is no work");
        assert_eq!(os.calls().len(), 3);
    }

    #[test]
    fn a_seeded_reconciler_applies_only_what_changed() {
        let r = EffectReconciler::seeded(&desired("Alt+Q", true, LaunchPlacement::Camera), true);
        let os = FakeOs::default();
        assert!(r.reconcile(|| desired("Alt+Q", true, LaunchPlacement::Camera), &os).is_noop());
        r.reconcile(|| desired("Alt+Q", false, LaunchPlacement::Camera), &os);
        assert_eq!(os.calls(), ["on_top false"]);
    }

    #[test]
    fn a_refused_hotkey_is_retried_by_the_next_run_until_it_takes() {
        // Another app owns Alt+T when the user saves it: the registration
        // fails and must NOT count as applied, or re-saving the same combo
        // after the other app lets go would never register it.
        let r = EffectReconciler::seeded(&desired("Alt+Q", true, LaunchPlacement::Remembered), true);
        let os = FakeOs::default();
        os.taken.lock().unwrap().push("Alt+T".into());
        let want = || desired("Alt+T", true, LaunchPlacement::Remembered);
        assert_eq!(r.reconcile(want, &os).hotkey.as_deref(), Some("Alt+T"));
        assert_eq!(os.state().0, None, "refused: nothing is registered");
        r.reconcile(want, &os); // a save while it is still taken: tried again
        os.taken.lock().unwrap().clear(); // the other app lets go
        r.reconcile(want, &os);
        assert_eq!(os.state().0.as_deref(), Some("Alt+T"));
        assert!(r.reconcile(want, &os).is_noop(), "once it took, it is applied");
        assert_eq!(os.calls(), ["hotkey Alt+T", "hotkey Alt+T", "hotkey Alt+T"]);
    }

    #[test]
    fn a_startup_hotkey_the_os_refused_is_retried_by_the_first_save() {
        let r = EffectReconciler::seeded(&desired("Alt+Q", true, LaunchPlacement::Remembered), false);
        let os = FakeOs::default();
        r.reconcile(|| desired("Alt+Q", true, LaunchPlacement::Remembered), &os);
        assert_eq!(os.calls(), ["hotkey Alt+Q"]);
        assert_eq!(os.state().0.as_deref(), Some("Alt+Q"));
    }

    #[test]
    fn docking_fires_on_the_transition_into_camera_only() {
        let r = EffectReconciler::seeded(&desired("", true, LaunchPlacement::Remembered), true);
        let os = FakeOs::default();
        r.reconcile(|| desired("", true, LaunchPlacement::Camera), &os);
        r.reconcile(|| desired("", true, LaunchPlacement::Camera), &os);
        r.reconcile(|| desired("", true, LaunchPlacement::Remembered), &os);
        r.reconcile(|| desired("", true, LaunchPlacement::Camera), &os);
        assert_eq!(os.calls(), ["dock", "dock"]);
    }

    #[test]
    fn effects_run_in_reverse_commit_order_still_leave_the_newest_hotkey() {
        // Commit A (hotkey X) then B (hotkey Y); B's effect run happens
        // before A's. The old per-patch diff would register Y, then X.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let r = seeded_for(&store);
        let os = FakeOs::default();
        store.apply_patch(hotkey("Alt+X")).unwrap();
        store.apply_patch(hotkey("Alt+Y")).unwrap();
        let current = || DesiredOsState::of(&store.get());
        r.reconcile(current, &os); // B's run
        r.reconcile(current, &os); // A's run, late
        assert_eq!(os.calls(), ["hotkey Alt+Y"], "the late run is a no-op, not a rollback");
        assert_eq!(os.state().0.as_deref(), Some("Alt+Y"));
        assert_eq!(store.get().hotkey, "Alt+Y");
    }

    #[test]
    fn a_later_style_only_commit_never_swallows_an_earlier_hotkey_change() {
        // A changes the hotkey, B only the style. If A's run is skipped (or
        // arrives after B's), B's run must still register A's hotkey:
        // "skip older effects" would leave the old combination live.
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let r = seeded_for(&store);
        let os = FakeOs::default();
        store.apply_patch(hotkey("Alt+A")).unwrap();
        store.apply_patch(style(AnswerStyle::Brief)).unwrap();
        r.reconcile(|| DesiredOsState::of(&store.get()), &os); // only B's run happens
        assert_eq!(os.calls(), ["hotkey Alt+A"]);
    }

    #[test]
    fn a_failed_persist_leaves_disk_memory_and_os_agreeing() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let r = EffectReconciler::new();
        let os = FakeOs::default();
        store.apply_patch(hotkey("Alt+1")).unwrap();
        r.reconcile(|| DesiredOsState::of(&store.get()), &os);
        assert_agree(dir.path(), &store, &os);

        // The next write fails (a directory squats on the tmp name).
        std::fs::create_dir(dir.path().join(crate::store::settings::SETTINGS_TMP_NAME)).unwrap();
        let before = store.revision();
        let err = store
            .apply_patch(SettingsPatch { always_on_top: Some(false), ..hotkey("Alt+2") })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        // The shell still runs the effect step after a failed save; it must
        // find nothing to do, because nothing was committed.
        assert!(r.reconcile(|| DesiredOsState::of(&store.get()), &os).is_noop());
        assert_eq!(store.revision(), before);
        assert_agree(dir.path(), &store, &os);
    }

    #[test]
    fn a_stale_form_overlapping_chip_saves_converges_after_reload() {
        let dir = tempdir().unwrap();
        let store = SettingsStore::load_from(dir.path());
        let r = seeded_for(&store);
        let os = FakeOs::default();
        let form: SettingsView = store.view(); // Settings opened here

        // A chip click commits while the form is open; its effect run is
        // delayed until after the form's attempt.
        store.apply_patch(style(AnswerStyle::Detailed)).unwrap();
        let full_form = |expected: u64| SettingsPatch {
            expected_revision: Some(expected),
            profiles: Some(vec![CallProfilePatch {
                id: "default".into(),
                name: "Default".into(),
                resume: "typed in the form".into(),
                ..Default::default()
            }]),
            answer_style: Some(form.answer_style),
            always_on_top: Some(false),
            ..hotkey("Alt+F")
        };
        let err = store.apply_patch(full_form(form.revision)).unwrap_err();
        assert_eq!(err.code, ErrorCode::SettingsConflict);
        r.reconcile(|| DesiredOsState::of(&store.get()), &os); // the chip's late run
        assert!(os.calls().is_empty(), "a style change has no OS effect");

        // Reload: the form re-seeds from the current view. The user kept
        // their typed text; the chip's style is taken from the new view.
        let current = store.view();
        let saved = store
            .apply_patch(SettingsPatch { answer_style: Some(current.answer_style), ..full_form(current.revision) })
            .unwrap();
        assert_eq!(saved.answer_style, AnswerStyle::Detailed);
        assert_eq!(saved.profiles[0].resume, "typed in the form");
        r.reconcile(|| DesiredOsState::of(&store.get()), &os);
        assert_eq!(os.calls(), ["hotkey Alt+F", "on_top false"]);
        assert_agree(dir.path(), &store, &os);
    }

    #[test]
    fn concurrent_commits_and_effect_runs_converge_in_any_interleaving() {
        // Each thread commits and then runs the effect step, as
        // set_settings does. Whatever the scheduler does, the last run to
        // take the lock read a state at least as new as every commit, so the
        // OS ends on the committed state with no extra step.
        for round in 0..20 {
            let dir = tempdir().unwrap();
            let store = Arc::new(SettingsStore::load_from(dir.path()));
            // Unseeded, so the fake OS state is always defined afterwards.
            let r = Arc::new(EffectReconciler::new());
            let os = Arc::new(FakeOs::default());
            let barrier = Arc::new(Barrier::new(4));
            let handles: Vec<_> = (0..4)
                .map(|i| {
                    let (store, r, os, barrier) =
                        (Arc::clone(&store), Arc::clone(&r), Arc::clone(&os), Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        barrier.wait();
                        let patch = if i % 2 == 0 {
                            SettingsPatch { always_on_top: Some(i == 0), ..hotkey(&format!("Alt+{i}")) }
                        } else {
                            style(AnswerStyle::Brief)
                        };
                        store.apply_patch(patch).unwrap();
                        r.reconcile(|| DesiredOsState::of(&store.get()), &*os);
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!(store.revision(), 5, "round {round}");
            assert_agree(dir.path(), &store, &os);
        }
    }
}
