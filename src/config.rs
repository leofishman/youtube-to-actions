use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub youtube: YoutubeConfig,
    pub processing: ProcessingConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoutubeConfig {
    /// Path to credentials.json (Google OAuth Desktop app credentials)
    pub credentials_path: Option<String>,
    /// ID of the private playlist to watch
    pub playlist_id: String,
    /// Optional: after processing, move videos to this playlist
    pub processed_playlist_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingConfig {
    /// Ollama endpoint (e.g. http://localhost:11434)
    pub ollama_url: Option<String>,
    /// Model name (e.g. "llama3.2")
    pub ollama_model: Option<String>,
    /// Alternative: use a remote API (OpenAI-compatible)
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    pub api_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Create tasks in Super Productivity
    pub sp_enabled: bool,
    /// SP project ID (default: "INBOX_PROJECT")
    pub sp_project_id: Option<String>,
    /// SP tag IDs for tech/learn/dev categories
    pub sp_tag_learn: Option<String>,
    pub sp_tag_dev: Option<String>,

    /// Create notes in Obsidian
    pub obsidian_vault: Option<String>,

    /// Category → Vault folder mapping
    /// Override defaults: tutorial, concept, tool, news, health, entertainment, other
    #[serde(default)]
    pub category_folders: std::collections::HashMap<String, String>,
}

impl Config {
    /// Load config from a TOML file, with defaults for optional fields
    pub fn from_file(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;
        let mut cfg: Config = toml::from_str(&content)
            .with_context(|| "Failed to parse config file")?;

        // Apply defaults
        cfg.output.sp_project_id = cfg.output.sp_project_id.or(Some("INBOX_PROJECT".into()));
        cfg.output.sp_tag_learn = cfg.output.sp_tag_learn.or(Some("learn".into()));
        cfg.output.sp_tag_dev = cfg.output.sp_tag_dev.or(Some("dev".into()));

        // Resolve credentials path relative to project root if relative
        if let Some(ref creds) = cfg.youtube.credentials_path {
            if !PathBuf::from(creds).is_absolute() {
                // Try to resolve relative to config file location
                if let Some(config_dir) = path.parent() {
                    let resolved = config_dir.join(creds);
                    if resolved.exists() {
                        cfg.youtube.credentials_path = Some(resolved.to_string_lossy().to_string());
                    }
                }
            }
        }

        Ok(cfg)
    }

    /// Default config path: ~/.config/yt2action/config.toml
    pub fn default_path() -> Result<PathBuf> {
        let dir = directories::ProjectDirs::from("com", "leofishman", "yt2action")
            .context("Failed to find config directory")?;
        Ok(dir.config_dir().join("config.toml"))
    }
}
