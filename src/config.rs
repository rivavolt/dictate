use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub struct Config {
    pub state_file: PathBuf,
    pub transcript_file: PathBuf,
    pub audio_file: PathBuf,
    pub lang_file: PathBuf,
    pub mode_file: PathBuf,
    pub sound_dir: PathBuf,
    pub socket_path: PathBuf,
}

impl Config {
    pub fn new() -> Self {
        let runtime_dir = PathBuf::from(
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string()),
        );
        let state_dir = dirs::state_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"))
            .join("dictate");
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));

        let _ = fs::create_dir_all(&state_dir);

        Self {
            state_file: runtime_dir.join("dictate.state"),
            transcript_file: runtime_dir.join("dictate.transcript"),
            audio_file: runtime_dir.join("dictate.wav"),
            lang_file: state_dir.join("language"),
            mode_file: state_dir.join("mode"),
            sound_dir: config_dir.join("dictate/sounds"),
            socket_path: runtime_dir.join("dictate.sock"),
        }
    }
}

pub fn get_api_key() -> Result<String> {
    std::env::var("DEEPGRAM_API_KEY")
        .or_else(|_| {
            let env_file = dirs::home_dir().unwrap_or_default().join(".config/env");
            if let Ok(content) = fs::read_to_string(&env_file) {
                for line in content.lines() {
                    if line.starts_with("export DEEPGRAM_API_KEY=") {
                        return Ok(line
                            .trim_start_matches("export DEEPGRAM_API_KEY=")
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string());
                    }
                }
            }
            Err(std::env::VarError::NotPresent)
        })
        .context("DEEPGRAM_API_KEY not set in env or ~/.config/env")
}

pub fn get_evdev_key() -> u16 {
    std::env::var("DICTATE_KEY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(63) // KEY_F5
}

#[derive(Clone)]
pub struct State {
    pub lang: String,
    pub model: String,
    pub mode: String,
}

impl State {
    pub fn load(config: &Config) -> Self {
        let lang = fs::read_to_string(&config.lang_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| {
                std::env::var("DICTATE_LANG").unwrap_or_else(|_| "multi".to_string())
            });

        let model = std::env::var("DICTATE_MODEL").unwrap_or_else(|_| "nova-3".to_string());

        let mode = fs::read_to_string(&config.mode_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "live".to_string());

        Self { lang, model, mode }
    }
}
