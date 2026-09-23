//! Window geometry restore/persist, navigation policy, and webview recovery
//! (§9, §11).

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize, Url, WebviewWindow, Window,
    WindowEvent,
};

use app_core::store::{dock_preset_size, dock_top_center, sanitize_bounds, RawBounds, WorkArea};
use app_core::AppError;

use crate::state::{lock, AppState};

pub const MAIN_WINDOW: &str = "main";
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
pub const RELOAD_MIN_GAP: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Capture exclusion (verified, not just requested)
// ---------------------------------------------------------------------------

/// `WDA_EXCLUDEFROMCAPTURE`: Windows leaves the window out of screen captures
/// made through its public capture APIs. Supported from Windows 10 version
/// 2004; older builds reject it.
pub const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;

/// Why launch is refused when the exclusion did not take. Names the Windows
/// requirement because that is the one cause a user can act on.
pub const MSG_CAPTURE_EXCLUSION_REFUSED: &str = "Windows did not apply screen-capture exclusion to the window, so the app will not start. This needs Windows 10 version 2004 or later.";

/// Map what `GetWindowDisplayAffinity` read back (`None` = the read itself
/// failed) to a launch verdict. Pure so the mapping is unit-tested; the real
/// window cannot be created under `cargo test`.
///
/// Only `WDA_EXCLUDEFROMCAPTURE` passes. `WDA_NONE` (0x00) means the request
/// was dropped, and `WDA_MONITOR` (0x01) is the pre-2004 fallback that shows
/// a black rectangle rather than excluding the window — neither is the
/// promise the app makes.
pub fn capture_exclusion_verdict(read_back: Option<u32>) -> Result<(), String> {
    match read_back {
        Some(WDA_EXCLUDEFROMCAPTURE) => Ok(()),
        Some(other) => Err(format!("{MSG_CAPTURE_EXCLUSION_REFUSED} (display affinity read back as 0x{other:02x})")),
        None => Err(format!("{MSG_CAPTURE_EXCLUSION_REFUSED} (the display affinity could not be read back)")),
    }
}

/// Read the window's display affinity back from Windows and require
/// `WDA_EXCLUDEFROMCAPTURE`.
///
/// Needed because `set_content_protected` cannot report a refusal: tao 0.35
/// discards the result of `SetWindowDisplayAffinity`, and the runtime only
/// reports a failure to send the request. Declared directly against user32
/// rather than pulling a `windows` crate version into the shell.
#[cfg(windows)]
pub fn verify_capture_exclusion(win: &WebviewWindow) -> Result<(), String> {
    #[link(name = "user32")]
    extern "system" {
        fn GetWindowDisplayAffinity(hwnd: *mut std::ffi::c_void, affinity: *mut u32) -> i32;
    }
    let hwnd = win
        .hwnd()
        .map_err(|e| format!("{MSG_CAPTURE_EXCLUSION_REFUSED} (no native window handle: {e})"))?;
    let mut affinity: u32 = 0;
    // SAFETY: `hwnd` is the live main window owned by this process, and
    // `affinity` is a valid out-pointer for the duration of the call.
    let ok = unsafe { GetWindowDisplayAffinity(hwnd.0, &mut affinity) };
    capture_exclusion_verdict((ok != 0).then_some(affinity))
}

/// Non-Windows builds exist only for tooling; there is nothing to verify.
#[cfg(not(windows))]
pub fn verify_capture_exclusion(_win: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Geometry restore
// ---------------------------------------------------------------------------

/// Apply saved geometry through `sanitize_bounds` against the monitors that
/// exist right now.
///
/// The size is applied only when it came from the settings file: those are
/// PHYSICAL pixels, saved by `current_bounds`, and round-trip exactly. The
/// first-run fallback is the builder's LOGICAL 460x700, which tao has already
/// scaled by DPI; re-applying it through `set_size(PhysicalSize)` shrank the
/// window by the scale factor on every 125-150 % laptop and then persisted
/// that smaller size as the user's "choice" (RS-7).
///
/// The position is applied only when the sanitizer could prove it lands on a
/// live display. Otherwise the window docks to the camera (§9: top-centre of
/// the current display, the spot the answer is designed to be read from) and
/// centres only if docking itself fails — centre is the one spot guaranteed
/// reachable when even the monitor list cannot be trusted.
pub fn restore_geometry(window: &WebviewWindow, saved: Option<RawBounds>) {
    let displays = current_work_areas(window);
    let sanitized = sanitize_bounds(saved, &displays);
    if sanitized.from_saved {
        let _ = window.set_size(PhysicalSize::new(sanitized.width, sanitized.height));
    }
    match sanitized.position {
        Some((x, y)) => {
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        None => {
            if dock_to_camera(window, DockSize::Keep).is_err() {
                let _ = window.center();
            }
        }
    }
}

fn work_area_of(m: &Monitor) -> WorkArea {
    // `work_area` already excludes the taskbar, in physical pixels — the
    // units every pure geometry function in the core speaks.
    let area = m.work_area();
    WorkArea {
        x: area.position.x,
        y: area.position.y,
        width: area.size.width,
        height: area.size.height,
    }
}

fn current_work_areas(window: &WebviewWindow) -> Vec<WorkArea> {
    // Enumeration failure yields an empty list, which sanitize_bounds treats as
    // "cannot prove anything, let the shell place it" — exactly the safe
    // answer.
    window
        .available_monitors()
        .map(|monitors| monitors.iter().map(work_area_of).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Dock to camera (§9)
// ---------------------------------------------------------------------------

/// Which size a dock applies: the wide reading preset (the header button) or
/// the size the window already has (launch with `launchPlacement: camera`,
/// and the restore fallback — the saved size is the user's; only the position
/// is overridden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSize {
    Preset,
    Keep,
}

fn os_err(e: tauri::Error) -> AppError {
    AppError::internal(format!("Could not move the window: {e}"))
}

/// Move the window to the top-centre of the display it is on (the primary
/// when that cannot be determined) — directly under a webcam, so reading the
/// answer reads as eye contact. The Moved/Resized events this raises go
/// through the normal debounced bounds save, so the docked geometry persists
/// exactly like a drag would. All math is in the core (`dock_top_center`,
/// `dock_preset_size`); this adapter only measures and applies.
pub fn dock_to_camera(window: &WebviewWindow, size: DockSize) -> Result<(), AppError> {
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| AppError::internal("Could not find the display this window is on."))?;
    let work = work_area_of(&monitor);

    let inner = window.inner_size().map_err(os_err)?;
    let outer = window.outer_size().map_err(os_err)?;
    // Measured BEFORE any resize: the decoration width is a property of the
    // window style, not of its current size, and reading outer_size straight
    // after a dispatched set_size would race the event loop when the caller
    // is not on the main thread (the async settings hop).
    let frame_w = outer.width.saturating_sub(inner.width);

    let (inner_w, inner_h) = match size {
        DockSize::Preset => dock_preset_size(work, monitor.scale_factor()),
        DockSize::Keep => (inner.width, inner.height),
    };
    if size == DockSize::Preset {
        window.set_size(PhysicalSize::new(inner_w, inner_h)).map_err(os_err)?;
    }
    // Windows' invisible resize borders sit inside GetWindowRect symmetrically,
    // so centring the OUTER rect centres the visible frame; the top border is
    // ~1 px, so the visible top lands DOCK_TOP_MARGIN below the work-area edge
    // (and below a top-docked taskbar, which `work.y` already excludes).
    let (x, y) = dock_top_center(work, inner_w + frame_w);
    window.set_position(PhysicalPosition::new(x, y)).map_err(os_err)
}

fn current_bounds(window: &Window) -> Option<RawBounds> {
    // A minimized window measures at (-32000,-32000) with a caption-sized
    // rect; persisting that would clobber the user's real geometry with
    // garbage (which the sanitizer then "recovers" to defaults). The last
    // debounced pending value is the honest fallback.
    if window.is_minimized().unwrap_or(false) {
        return None;
    }
    let pos = window.outer_position().ok()?;
    // INNER size, deliberately: restore applies through set_size, which
    // tauri-runtime-wry implements as tao's set_inner_size. Saving the outer
    // rect grows the client area by the decoration size (~16×39 px) on every
    // launch — a window that monotonically creeps larger forever.
    let size = window.inner_size().ok()?;
    Some(RawBounds {
        x: pos.x as f64,
        y: pos.y as f64,
        width: size.width as f64,
        height: size.height as f64,
    })
}

// ---------------------------------------------------------------------------
// Geometry persist: debounced on move/resize, flushed on close
// ---------------------------------------------------------------------------

/// Debounce bookkeeping for geometry saves. Each move/resize records the
/// newest bounds and takes a generation token; the timer task that still holds
/// the newest token performs the save, every superseded task finds its token
/// stale and does nothing. Pure so the supersession logic is unit-testable
/// without a window or a clock.
pub struct DebouncedSaver {
    generation: AtomicU64,
    pending: Mutex<Option<RawBounds>>,
}

impl DebouncedSaver {
    pub fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            pending: Mutex::new(None),
        }
    }

    /// Record the latest geometry; returns the token identifying this update.
    pub fn touch(&self, bounds: RawBounds) -> u64 {
        *lock(&self.pending) = Some(bounds);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Claim the pending save if `token` is still the newest update. A stale
    /// token means another move superseded this one — its timer owns the save
    /// now, and saving here would write geometry the user already moved past.
    pub fn take_if_current(&self, token: u64) -> Option<RawBounds> {
        if self.generation.load(Ordering::SeqCst) != token {
            return None;
        }
        lock(&self.pending).take()
    }

    /// Claim whatever is pending regardless of freshness — the close-time
    /// flush, for when the window is already gone and cannot be re-measured.
    pub fn take_pending(&self) -> Option<RawBounds> {
        lock(&self.pending).take()
    }
}

pub fn handle_window_event(window: &Window, event: &WindowEvent) {
    match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => schedule_bounds_save(window),
        // Flush on BOTH close paths: some paths (WM_ENDSESSION, programmatic
        // destroy) skip CloseRequested, and the debounced save is what makes a
        // kill mid-drag keep the geometry. Belt and braces by design.
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => flush_bounds(window),
        _ => {}
    }
}

fn schedule_bounds_save(window: &Window) {
    let Some(state) = window.try_state::<AppState>() else {
        return;
    };
    let Some(bounds) = current_bounds(window) else {
        return;
    };
    let token = state.bounds_saver.touch(bounds);
    let saver = Arc::clone(&state.bounds_saver);
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SAVE_DEBOUNCE).await;
        if let Some(bounds) = saver.take_if_current(token) {
            save_bounds(&app, bounds);
        }
    });
}

fn flush_bounds(window: &Window) {
    let Some(state) = window.try_state::<AppState>() else {
        return;
    };
    // Prefer a fresh measurement; once the native window is gone fall back to
    // the last debounced value so a close mid-debounce still lands.
    let bounds = current_bounds(window).or_else(|| state.bounds_saver.take_pending());
    if let Some(bounds) = bounds {
        save_bounds(&window.app_handle().clone(), bounds);
    }
}

fn save_bounds(app: &AppHandle, bounds: RawBounds) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Geometry is cosmetic data. A failed — or panicking — write during
    // shutdown must never take the process down or block the close, so both
    // the error and any panic are swallowed here on purpose.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        lock(&state.settings).save_window_bounds(bounds);
    }));
}

// ---------------------------------------------------------------------------
// Navigation policy: the app never navigates; https opens externally
// ---------------------------------------------------------------------------

/// The only origins the app itself may load: the bundled asset protocol and
/// the Vite dev server. Everything else never renders in-app.
pub fn is_internal_origin(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    match scheme {
        "tauri" => true,
        // Windows serves the bundled app over the tauri.localhost pseudo-host.
        "http" | "https" => match host {
            Some("tauri.localhost") => true,
            Some("localhost") => port == Some(5173),
            _ => false,
        },
        _ => false,
    }
}

/// https ONLY, and nothing that could split into a second process argument.
/// The URL is handed to the OS as one argv entry — control characters,
/// whitespace and quotes are how one argument stops being one argument.
pub fn is_safe_external_url(url: &str) -> bool {
    let Some(scheme) = url.get(..8) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https://") || url.len() <= 8 {
        return false;
    }
    !url.chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// `Builder::on_navigation` hook: allow in-app origins, bounce https to the
/// default browser, silently drop everything else (file:, javascript:, http:).
pub fn handle_navigation(url: &Url) -> bool {
    if is_internal_origin(url.scheme(), url.host_str(), url.port()) {
        return true;
    }
    if is_safe_external_url(url.as_str()) {
        open_in_browser(url.as_str());
    }
    false
}

pub fn open_in_browser(url: &str) {
    debug_assert!(is_safe_external_url(url));
    #[cfg(windows)]
    {
        // explorer.exe hands an https URL to the default browser. The URL is
        // passed as a single argument — never through cmd.exe — so it cannot
        // be split, quoted apart, or chained into a second command.
        let _ = std::process::Command::new("explorer.exe").arg(url).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

// ---------------------------------------------------------------------------
// Webview crash recovery
// ---------------------------------------------------------------------------

/// Rate limiter for webview reloads: a page that crashes on boot would
/// otherwise reload in a tight flicker loop forever. Denied attempts do NOT
/// push the window: a burst of crashes must not delay the next honest retry.
pub struct ReloadLimiter {
    min_gap: Duration,
    last: Option<Instant>,
}

impl ReloadLimiter {
    pub fn new(min_gap: Duration) -> Self {
        Self { min_gap, last: None }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(prev) if now.duration_since(prev) < self.min_gap => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Reload the webview after a renderer crash, at most once per
/// `RELOAD_MIN_GAP`. Fired from WebView2's ProcessFailed event (see
/// `install_crash_recovery`); a page that crashes on boot reloads once per
/// gap instead of flickering forever.
pub fn recover_webview(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if !lock(&state.reload_limiter).allow(Instant::now()) {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.reload();
    }
}

/// Attach the crash *signal*: WebView2's ProcessFailed event, reached through
/// the raw COM controller because Tauri 2 does not surface it. Best-effort by
/// design — if any step fails the app still runs, just without automatic
/// recovery from a renderer crash.
#[cfg(windows)]
pub fn install_crash_recovery(win: &WebviewWindow) {
    use webview2_com::ProcessFailedEventHandler;

    let app = win.app_handle().clone();
    let _ = win.with_webview(move |webview| {
        // SAFETY: `with_webview` runs this on the main thread with a live
        // controller; the COM calls are plain event registration.
        unsafe {
            let controller = webview.controller();
            let Ok(core) = controller.CoreWebView2() else {
                return;
            };
            let handler = ProcessFailedEventHandler::create(Box::new(move |_sender, _args| {
                // The event arrives on a WebView2 thread; window operations
                // belong on the main thread, and the rate limiter makes the
                // hop idempotent under a crash storm.
                let app = app.clone();
                let _ = app.clone().run_on_main_thread(move || recover_webview(&app));
                Ok(())
            }));
            // webview2-com 0.38 types the registration token as a bare i64.
            let mut token = 0i64;
            let _ = core.add_ProcessFailed(&handler, &mut token);
        }
    });
}

#[cfg(not(windows))]
pub fn install_crash_recovery(_win: &WebviewWindow) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exclude_from_capture_counts_as_verified() {
        // The launch promise is "excluded from capture", checked against
        // what Windows actually applied. WDA_MONITOR is the pre-2004
        // black-rectangle mode — not the promise — and a failed read proves
        // nothing, so both refuse, with the Windows requirement named.
        assert_eq!(capture_exclusion_verdict(Some(0x11)), Ok(()));
        for refused in [Some(0x00), Some(0x01), None] {
            let err = capture_exclusion_verdict(refused).unwrap_err();
            assert!(err.contains("Windows 10 version 2004"), "{err}");
        }
    }

    fn bounds(x: f64) -> RawBounds {
        RawBounds { x, y: 0.0, width: 460.0, height: 700.0 }
    }

    // --- debounce ---

    #[test]
    fn latest_touch_owns_the_save_and_stale_tokens_get_nothing() {
        let saver = DebouncedSaver::new();
        let first = saver.touch(bounds(1.0));
        let second = saver.touch(bounds(2.0));

        // The superseded timer wakes first and must not save bounds(1.0) —
        // the user has already moved past it.
        assert_eq!(saver.take_if_current(first), None);
        // The newest timer gets the newest bounds.
        assert_eq!(saver.take_if_current(second), Some(bounds(2.0)));
        // And the save is one-shot.
        assert_eq!(saver.take_if_current(second), None);
    }

    #[test]
    fn a_stale_wakeup_does_not_consume_the_pending_bounds() {
        let saver = DebouncedSaver::new();
        let first = saver.touch(bounds(1.0));
        let second = saver.touch(bounds(2.0));

        assert_eq!(saver.take_if_current(first), None);
        // The stale claim above must have left the pending value for the
        // rightful owner.
        assert_eq!(saver.take_if_current(second), Some(bounds(2.0)));
    }

    #[test]
    fn close_flush_takes_pending_regardless_of_generation() {
        let saver = DebouncedSaver::new();
        saver.touch(bounds(1.0));
        saver.touch(bounds(3.0));
        assert_eq!(saver.take_pending(), Some(bounds(3.0)));
        assert_eq!(saver.take_pending(), None);
    }

    // --- reload limiter ---

    #[test]
    fn first_reload_is_allowed_immediately() {
        let mut lim = ReloadLimiter::new(Duration::from_secs(10));
        assert!(lim.allow(Instant::now()));
    }

    #[test]
    fn reloads_inside_the_gap_are_denied() {
        let t0 = Instant::now();
        let mut lim = ReloadLimiter::new(Duration::from_secs(10));
        assert!(lim.allow(t0));
        assert!(!lim.allow(t0 + Duration::from_secs(1)));
        assert!(!lim.allow(t0 + Duration::from_secs(9)));
    }

    #[test]
    fn a_reload_after_the_gap_is_allowed_again() {
        let t0 = Instant::now();
        let mut lim = ReloadLimiter::new(Duration::from_secs(10));
        assert!(lim.allow(t0));
        assert!(lim.allow(t0 + Duration::from_secs(10)));
    }

    #[test]
    fn denied_attempts_do_not_push_the_window() {
        // A crash storm at t+1..t+9 must not starve the retry at t+10.
        let t0 = Instant::now();
        let mut lim = ReloadLimiter::new(Duration::from_secs(10));
        assert!(lim.allow(t0));
        for s in 1..10 {
            assert!(!lim.allow(t0 + Duration::from_secs(s)));
        }
        assert!(lim.allow(t0 + Duration::from_secs(10)));
    }

    // --- external link guard ---

    #[test]
    fn https_urls_are_allowed() {
        assert!(is_safe_external_url("https://example.com/docs?q=1"));
        // Scheme comparison is case-insensitive per RFC 3986.
        assert!(is_safe_external_url("HTTPS://example.com"));
    }

    #[test]
    fn non_https_schemes_are_rejected() {
        assert!(!is_safe_external_url("http://example.com"));
        assert!(!is_safe_external_url("file:///C:/Windows"));
        assert!(!is_safe_external_url("javascript:alert(1)"));
        assert!(!is_safe_external_url("mailto:a@b.c"));
        assert!(!is_safe_external_url(""));
        assert!(!is_safe_external_url("https://"));
    }

    #[test]
    fn urls_that_could_split_an_argument_are_rejected() {
        assert!(!is_safe_external_url("https://example.com/a b"));
        assert!(!is_safe_external_url("https://example.com/\"x"));
        assert!(!is_safe_external_url("https://example.com/\n"));
        assert!(!is_safe_external_url("https://example.com/\t"));
        assert!(!is_safe_external_url("https://example.com/\u{0007}"));
    }

    #[test]
    fn multibyte_prefixes_do_not_panic_the_guard() {
        // A non-ASCII char straddling byte 8 would panic a naive `url[..8]`.
        assert!(!is_safe_external_url("httpsé//x"));
        assert!(!is_safe_external_url("née"));
    }

    // --- shipped CSP ---

    #[test]
    fn the_production_csp_keeps_styles_locked_down() {
        // WHY: index.html's meta CSP carries style-src 'unsafe-inline' for
        // Vite's dev-time style injection, and the effective policy is the
        // intersection of the two. tauri.conf.json's csp is therefore the ONLY
        // thing keeping inline styles out of the shipped app — deleting the
        // entry (or "simplifying" it to match the meta) would silently reopen
        // 'unsafe-inline' in production. Nothing else pins this file.
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid tauri.conf.json");
        let csp = conf["app"]["security"]["csp"].as_str().expect("csp is configured");
        assert!(csp.contains("style-src 'self'"), "csp was: {csp}");
        assert!(!csp.contains("unsafe-inline"), "csp was: {csp}");
        assert!(csp.contains("default-src 'self'"), "csp was: {csp}");
        assert!(csp.contains("script-src 'self'"), "csp was: {csp}");
    }

    // --- internal origin classification ---

    #[test]
    fn app_origins_are_internal() {
        assert!(is_internal_origin("tauri", Some("localhost"), None));
        assert!(is_internal_origin("http", Some("tauri.localhost"), None));
        assert!(is_internal_origin("https", Some("tauri.localhost"), None));
        assert!(is_internal_origin("http", Some("localhost"), Some(5173)));
    }

    #[test]
    fn everything_else_is_external() {
        assert!(!is_internal_origin("https", Some("example.com"), None));
        // The dev-server host without the dev port is NOT a free pass — a page
        // on another local service is still another page.
        assert!(!is_internal_origin("http", Some("localhost"), Some(8080)));
        assert!(!is_internal_origin("http", Some("localhost"), None));
        assert!(!is_internal_origin("file", None, None));
    }
}
