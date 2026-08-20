//! AI Call Assistant core.
//!
//! This crate owns the entire pipeline — audio capture, speech-to-text, LLM
//! streaming, the session state machine, settings and secrets — and knows
//! nothing about Tauri. That separation is what lets `cargo test -p app-core`
//! exercise every invariant in the spec with fakes, without a webview, a
//! network, or an audio device.

pub mod audio;
pub mod error;
pub mod llm;
pub mod session;
pub mod stt;
pub mod store;

pub use error::{AppError, AppResult, ErrorCode};
