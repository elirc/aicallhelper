//! Explicit local-service readiness and startup; downloads belong to setup.
use crate::commands::Envelope;
use app_core::{
    llm::local::{self, MODEL, ORIGIN},
    AppError,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalVoiceStatus {
    pub ollama_running: bool,
    pub model_available: bool,
    pub speech_ready: bool,
}

pub async fn status() -> LocalVoiceStatus {
    let ollama = async {
        let Ok(response) = local::client()
            .get(format!("{ORIGIN}/api/tags"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        else {
            return (false, false);
        };
        if !response.status().is_success() {
            return (false, false);
        }
        let Ok(body) = response.json::<serde_json::Value>().await else {
            return (false, false);
        };
        let Some(models) = body["models"].as_array() else {
            return (false, false);
        };
        (
            true,
            models
                .iter()
                .any(|m| m["name"] == MODEL || m["model"] == MODEL),
        )
    };
    let speech = async {
        let Ok(response) = local::client()
            .get("http://127.0.0.1:8765/health")
            .timeout(Duration::from_secs(3))
            .send()
            .await
        else {
            return false;
        };
        if !response.status().is_success() {
            return false;
        }
        let Ok(body) = response.json::<serde_json::Value>().await else {
            return false;
        };
        body["service"] == "callhelper-local-speech"
            && body["protocol"] == 1
            && body["ready"] == true
    };
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    data_dir: PathBuf,
}

fn config() -> Result<Config, AppError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| AppError::internal("Windows local app data is unavailable."))?;
    let path = base.join("AI Call Assistant").join("local-voice.json");
    let text = std::fs::read_to_string(path).map_err(|_| setup_error())?;
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

fn launch(
    executable: &Path,
    args: &[&std::ffi::OsStr],
    home: &Path,
    speech: bool,
) -> Result<(), AppError> {
    let mut cmd = Command::new(executable);
    cmd.args(args)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let log_name = if speech { "speech.log" } else { "ollama.log" };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(log_name))
        .map_err(|_| {
            AppError::internal("Could not open the local service log. Check the setup folder.")
        })?;
    cmd.stderr(log);
    if !speech {
        cmd.env("OLLAMA_HOST", "127.0.0.1:11434")
            .env("OLLAMA_NO_CLOUD", "1")
            .env("OLLAMA_MODELS", home.join("models").join("ollama"));
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
    if !current.ollama_running || !current.speech_ready {
        let config = config()?;
        let home = &config.data_dir;
        if !current.ollama_running {
            let exe = home.join("ollama").join("ollama.exe");
            launch(&exe, &[std::ffi::OsStr::new("serve")], home, false)?;
        }
        if !current.speech_ready {
            let python = home.join("venv").join("Scripts").join("python.exe");
            let server = home.join("server.py");
            launch(
                &python,
                &[
                    std::ffi::OsStr::new("-u"),
                    server.as_os_str(),
                    std::ffi::OsStr::new("--home"),
                    home.as_os_str(),
                ],
                home,
                true,
            )?;
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut current = status().await;
    while (!current.ollama_running || !current.speech_ready)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(500)).await;
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
