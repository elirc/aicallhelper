//! Explicit local-service readiness and startup; downloads belong to setup.
//!
//! Split on the I/O seam (R11): everything that DECIDES — what Ollama's tag
//! list says, whether the speech service's health reply is ours, what the
//! setup config means, which services a `prepare` must launch and how — is a
//! pure function with unit tests. The async wrappers at the bottom only move
//! bytes and spawn processes, because this is the one shell module that
//! starts executables from a JSON-configured path, and the decision logic is
//! what must not drift silently.

use crate::commands::Envelope;
use app_core::{
    llm::local::{self, MODEL, ORIGIN},
    AppError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

// ---------------------------------------------------------------------------
// Constants shared (by value) with the core and the setup script
// ---------------------------------------------------------------------------

/// Loopback port of the bundled Moonshine speech service. The core's
/// `stt::local::URL` dials the same port; a test below pins the two together
/// so the health probe and the transcript socket can never drift apart.
pub const SPEECH_PORT: u16 = 8765;
/// What a healthy speech service identifies itself as (server.py), and the
/// wire protocol this build speaks.
pub const SPEECH_SERVICE_NAME: &str = "callhelper-local-speech";
pub const SPEECH_PROTOCOL: u64 = 1;
/// `%LOCALAPPDATA%\AI Call Assistant\local-voice.json`, written by
/// scripts/setup-free-voice.ps1 — the only thing the app reads to find the
/// setup folder.
pub const CONFIG_DIR_NAME: &str = "AI Call Assistant";
pub const CONFIG_FILE_NAME: &str = "local-voice.json";

/// A readiness probe that has not answered in 3 s is talking to a service
/// that is down; the UI polls, so waiting longer only delays the next poll.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Cold start budget: Ollama plus a Python venv loading Moonshine on a
/// laptop. Past this the user is told which log to read.
const PREPARE_DEADLINE: Duration = Duration::from_secs(60);
const PREPARE_POLL: Duration = Duration::from_millis(500);

fn speech_health_url() -> String {
    format!("http://127.0.0.1:{SPEECH_PORT}/health")
}

/// Ollama's `OLLAMA_HOST` is `ORIGIN` without the scheme — derived rather
/// than repeated so the service can never be started on a port the client
/// does not dial.
fn ollama_host() -> &'static str {
    ORIGIN.trim_start_matches("http://")
}

// ---------------------------------------------------------------------------
// Wire shape
// ---------------------------------------------------------------------------

/// Mirrors `LocalVoiceStatus` in `src/types.ts` field for field (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalVoiceStatus {
    pub ollama_running: bool,
    pub model_available: bool,
    pub speech_ready: bool,
}

// ---------------------------------------------------------------------------
// Pure decisions
// ---------------------------------------------------------------------------

/// Ollama `/api/tags` → (the service answered with a model list, our model
/// is in it). Anything that is not a `models` array counts as "not running":
/// a proxy page or an error object must not read as a healthy service. Older
/// Ollama builds report the tag under `name`, newer ones under `model`;
/// either counts.
pub fn parse_tags(body: &Value) -> (bool, bool) {
    let Some(models) = body["models"].as_array() else {
        return (false, false);
    };
    (
        true,
        models.iter().any(|m| m["name"] == MODEL || m["model"] == MODEL),
    )
}

/// Speech `/health` → ready. All three fields must match: `service` proves
/// the port is ours and not some other local server, `protocol` that it is
/// the version this build speaks, `ready` that the models have loaded.
pub fn parse_health(body: &Value) -> bool {
    body["service"] == SPEECH_SERVICE_NAME
        && body["protocol"] == SPEECH_PROTOCOL
        && body["ready"] == true
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub data_dir: PathBuf,
}

/// Parse `local-voice.json`. PowerShell's `Out-File` writes a UTF-8 BOM that
/// serde_json rejects, so it is stripped first. A relative `dataDir` is
/// refused: every executable below is resolved under it, and "relative to
/// whatever the current directory happens to be" is how a launcher runs the
/// wrong binary. Every failure is the one setup error — the fix is the same.
pub fn parse_config(text: &str) -> Result<Config, AppError> {
    let config: Config =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| setup_error())?;
    if !config.data_dir.is_absolute() {
        return Err(setup_error());
    }
    Ok(config)
}

fn setup_error() -> AppError {
    AppError::internal("Free voice is not installed yet. Run scripts\\setup-free-voice.ps1 from the project folder, then retry. Setup requires Python and several GB of free disk space.")
}

/// The two local services, in the order `prepare` starts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Ollama,
    Speech,
}

/// Which services a `prepare` must launch, given what is already up. Only
/// the missing ones: a second `ollama serve` against a running service exits
/// on the bound port and litters the log. Ollama first — it is the slower of
/// the two to come up and the model warm at the end waits on it. A model that
/// is missing is not launchable; that is a setup error reported after the
/// wait.
pub fn launch_plan(status: &LocalVoiceStatus) -> Vec<Service> {
    let mut plan = Vec::new();
    if !status.ollama_running {
        plan.push(Service::Ollama);
    }
    if !status.speech_ready {
        plan.push(Service::Speech);
    }
    plan
}

/// Everything one launch needs, resolved under the setup folder — pure, so
/// the paths and the environment can be pinned without spawning anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    /// Appended to under `home`; stderr goes here so a crash is diagnosable.
    pub log_name: &'static str,
    pub env: Vec<(&'static str, OsString)>,
}

pub fn launch_spec(service: Service, home: &Path) -> LaunchSpec {
    match service {
        Service::Ollama => LaunchSpec {
            executable: home.join("ollama").join("ollama.exe"),
            args: vec!["serve".into()],
            log_name: "ollama.log",
            env: vec![
                // Loopback only, never a cloud fallback, and the models where
                // setup put them rather than the user's global Ollama store.
                ("OLLAMA_HOST", OsString::from(ollama_host())),
                ("OLLAMA_NO_CLOUD", OsString::from("1")),
                ("OLLAMA_MODELS", home.join("models").join("ollama").into_os_string()),
            ],
        },
        Service::Speech => LaunchSpec {
            executable: home.join("venv").join("Scripts").join("python.exe"),
            args: vec![
                // Unbuffered, so speech.log shows the crash line that
                // preceded a silent exit.
                "-u".into(),
                home.join("server.py").into_os_string(),
                "--home".into(),
                home.as_os_str().to_owned(),
            ],
            log_name: "speech.log",
            env: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// I/O wrappers
// ---------------------------------------------------------------------------

/// GET `url` on the loopback client: the JSON body on a 2xx, `None` for a
/// refused connection, a non-2xx status or a body that is not JSON — all of
/// which mean "not ready" to every caller.
async fn probe(url: &str) -> Option<Value> {
    let response = local::client().get(url).timeout(PROBE_TIMEOUT).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

pub async fn status() -> LocalVoiceStatus {
    let ollama = async {
        match probe(&format!("{ORIGIN}/api/tags")).await {
            Some(body) => parse_tags(&body),
            None => (false, false),
        }
    };
    let speech = async { probe(&speech_health_url()).await.is_some_and(|body| parse_health(&body)) };
    let ((ollama_running, model_available), speech_ready) = tokio::join!(ollama, speech);
    LocalVoiceStatus {
        ollama_running,
        model_available,
        speech_ready,
    }
}

#[tauri::command]
pub async fn local_voice_status() -> Envelope<LocalVoiceStatus> {
    Envelope::ok(status().await)
}

fn config_path() -> Result<PathBuf, AppError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| AppError::internal("Windows local app data is unavailable."))?;
    Ok(base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
}

fn load_config() -> Result<Config, AppError> {
    let text = std::fs::read_to_string(config_path()?).map_err(|_| setup_error())?;
    parse_config(&text)
}

fn launch(spec: &LaunchSpec, home: &Path) -> Result<(), AppError> {
    let mut cmd = Command::new(&spec.executable);
    cmd.args(&spec.args)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(spec.log_name))
        .map_err(|_| {
            AppError::internal("Could not open the local service log. Check the setup folder.")
        })?;
    cmd.stderr(log);
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map_err(|_| setup_error())?;
    Ok(())
}

#[tauri::command]
pub async fn prepare_local_voice() -> Envelope<LocalVoiceStatus> {
    Envelope::from_result(prepare().await)
}

async fn prepare() -> Result<LocalVoiceStatus, AppError> {
    static PREPARE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = PREPARE
        .try_lock()
        .map_err(|_| AppError::internal("Free mode is already starting. Please wait."))?;
    let current = status().await;
    let plan = launch_plan(&current);
    if !plan.is_empty() {
        let config = load_config()?;
        let home = &config.data_dir;
        for service in plan {
            launch(&launch_spec(service, home), home)?;
        }
    }
    let deadline = tokio::time::Instant::now() + PREPARE_DEADLINE;
    let mut current = status().await;
    while !launch_plan(&current).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(PREPARE_POLL).await;
        current = status().await;
    }
    if !current.speech_ready {
        return Err(AppError::internal("Local speech did not start. Check speech.log in your free voice setup folder and rerun setup if the models are missing."));
    }
    if !current.model_available {
        return Err(AppError::internal("Qwen3.5 2B is not installed in the running Ollama service. Run free voice setup to finish the download."));
    }
    local::warm().await?;
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status_of(ollama_running: bool, model_available: bool, speech_ready: bool) -> LocalVoiceStatus {
        LocalVoiceStatus { ollama_running, model_available, speech_ready }
    }

    // --- Ollama tag list ---

    #[test]
    fn parse_tags_finds_the_model_under_either_key() {
        // Older Ollama builds report `name`, newer ones `model`; a build that
        // only checked one would tell the user to reinstall a model they have.
        let by_name = json!({ "models": [{ "name": MODEL }] });
        assert_eq!(parse_tags(&by_name), (true, true));
        let by_model = json!({ "models": [{ "model": MODEL, "name": "qwen3.5:2b-something-else" }] });
        assert_eq!(parse_tags(&by_model), (true, true));
    }

    #[test]
    fn parse_tags_distinguishes_running_without_our_model_from_not_running() {
        // The two answers lead to different user actions: "finish the
        // download" versus "start the service".
        let other_models = json!({ "models": [{ "name": "llama3:8b" }, { "model": "qwen3.5:9b" }] });
        assert_eq!(parse_tags(&other_models), (true, false));
        assert_eq!(parse_tags(&json!({ "models": [] })), (true, false));

        // Anything that is not a model list is not Ollama answering: a proxy
        // page, an error object, or a bare string must never read as running.
        assert_eq!(parse_tags(&json!({ "error": "loading" })), (false, false));
        assert_eq!(parse_tags(&json!({ "models": "qwen3.5:2b" })), (false, false));
        assert_eq!(parse_tags(&json!("<html>")), (false, false));
        assert_eq!(parse_tags(&Value::Null), (false, false));
    }

    // --- speech health ---

    #[test]
    fn parse_health_needs_service_protocol_and_ready_together() {
        let healthy = json!({ "service": SPEECH_SERVICE_NAME, "protocol": SPEECH_PROTOCOL, "ready": true });
        assert!(parse_health(&healthy));

        // Each field alone is a distinct wrong answer: another local server
        // on the port, a protocol this build cannot speak, models still
        // loading. None may count as ready.
        let wrong_service = json!({ "service": "something-else", "protocol": 1, "ready": true });
        assert!(!parse_health(&wrong_service));
        let wrong_protocol = json!({ "service": SPEECH_SERVICE_NAME, "protocol": 2, "ready": true });
        assert!(!parse_health(&wrong_protocol));
        let not_ready = json!({ "service": SPEECH_SERVICE_NAME, "protocol": 1, "ready": false });
        assert!(!parse_health(&not_ready));
        // Type confusion is not a match either: "1" is not protocol 1 and
        // "true" is not ready.
        let stringly = json!({ "service": SPEECH_SERVICE_NAME, "protocol": "1", "ready": "true" });
        assert!(!parse_health(&stringly));
        assert!(!parse_health(&json!({})));
        assert!(!parse_health(&Value::Null));
    }

    #[test]
    fn the_health_probe_targets_the_port_the_core_dials() {
        // The shell probes /health and the core opens /transcribe; if the
        // ports ever diverged the panel would report "ready" for a service
        // the recording cannot reach.
        let core_url = app_core::stt::local::URL;
        assert!(
            core_url.contains(&format!("127.0.0.1:{SPEECH_PORT}/")),
            "core dials {core_url}, shell probes port {SPEECH_PORT}"
        );
        assert_eq!(speech_health_url(), format!("http://127.0.0.1:{SPEECH_PORT}/health"));
    }

    // --- setup config ---

    fn absolute_home() -> PathBuf {
        // An absolute path on THIS platform, so the test does not hard-code a
        // drive letter.
        std::env::temp_dir().join("free-voice")
    }

    #[test]
    fn parse_config_accepts_an_absolute_data_dir_with_or_without_a_bom() {
        let home = absolute_home();
        let plain = json!({ "dataDir": home }).to_string();
        assert_eq!(parse_config(&plain).unwrap(), Config { data_dir: home.clone() });
        // PowerShell's Out-File prepends U+FEFF; serde_json rejects it, and
        // a setup that "worked" would then never be found.
        let with_bom = format!("\u{feff}{plain}");
        assert_eq!(parse_config(&with_bom).unwrap(), Config { data_dir: home });
    }

    #[test]
    fn parse_config_rejects_relative_paths_and_garbage_with_the_setup_error() {
        // A relative dataDir would resolve ollama.exe against whatever the
        // current directory is — the wrong binary, or a planted one.
        for bad in [
            json!({ "dataDir": "free-voice" }).to_string(),
            json!({ "dataDir": "./free-voice" }).to_string(),
            json!({ "dataDir": "" }).to_string(),
            json!({ "dataDir": 7 }).to_string(),
            json!({}).to_string(),
            "not json".to_string(),
            String::new(),
        ] {
            let err = parse_config(&bad).unwrap_err();
            assert_eq!(err, setup_error(), "accepted: {bad:?}");
        }
        assert_eq!(
            setup_error().message,
            "Free voice is not installed yet. Run scripts\\setup-free-voice.ps1 from the project folder, then retry. Setup requires Python and several GB of free disk space."
        );
    }

    // --- launch plan ---

    #[test]
    fn launch_plan_starts_only_what_is_missing_ollama_first() {
        assert_eq!(launch_plan(&status_of(false, false, false)), vec![Service::Ollama, Service::Speech]);
        assert_eq!(launch_plan(&status_of(true, true, false)), vec![Service::Speech]);
        assert_eq!(launch_plan(&status_of(false, false, true)), vec![Service::Ollama]);
        // Nothing to launch when both are up — a second `ollama serve` would
        // fail on the bound port and litter the log.
        assert!(launch_plan(&status_of(true, true, true)).is_empty());
        // A missing model is not launchable: Ollama is up, the download is
        // the user's job, and `prepare` reports it after the wait.
        assert!(launch_plan(&status_of(true, false, true)).is_empty());
    }

    #[test]
    fn launch_specs_resolve_everything_under_the_setup_folder() {
        let home = absolute_home();

        let ollama = launch_spec(Service::Ollama, &home);
        assert_eq!(ollama.executable, home.join("ollama").join("ollama.exe"));
        assert_eq!(ollama.args, vec![OsString::from("serve")]);
        assert_eq!(ollama.log_name, "ollama.log");

        let speech = launch_spec(Service::Speech, &home);
        assert_eq!(speech.executable, home.join("venv").join("Scripts").join("python.exe"));
        assert_eq!(
            speech.args,
            vec![
                OsString::from("-u"),
                home.join("server.py").into_os_string(),
                OsString::from("--home"),
                home.as_os_str().to_owned(),
            ]
        );
        assert_eq!(speech.log_name, "speech.log");
        // The speech service inherits the environment untouched; only Ollama
        // needs steering.
        assert!(speech.env.is_empty());
    }

    #[test]
    fn ollama_is_pinned_to_loopback_with_no_cloud_and_local_models() {
        let home = absolute_home();
        let env = launch_spec(Service::Ollama, &home).env;
        let get = |k: &str| env.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());

        // The host the service binds must be the one the client dials —
        // derived from the same ORIGIN, so this cannot drift.
        assert_eq!(get("OLLAMA_HOST"), Some(OsString::from("127.0.0.1:11434")));
        assert_eq!(format!("http://{}", ollama_host()), ORIGIN);
        // Free local mode never falls back to a hosted model (§6.5).
        assert_eq!(get("OLLAMA_NO_CLOUD"), Some(OsString::from("1")));
        assert_eq!(
            get("OLLAMA_MODELS"),
            Some(home.join("models").join("ollama").into_os_string())
        );
        assert_eq!(env.len(), 3);
    }

    // --- wire shape ---

    #[test]
    fn local_voice_status_serializes_the_wire_shape() {
        // Pinned against `LocalVoiceStatus` in src/types.ts.
        let v = serde_json::to_value(status_of(true, false, true)).unwrap();
        assert_eq!(v, json!({ "ollamaRunning": true, "modelAvailable": false, "speechReady": true }));
    }
}
