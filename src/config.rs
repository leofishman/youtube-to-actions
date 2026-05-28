use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub youtube: YoutubeConfig,
    pub processing: ProcessingConfig,
    pub output: OutputConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoutubeConfig {
    /// Path to credentials.json (Google OAuth Desktop app credentials)
    pub credentials_path: Option<String>,
    /// ID of the private playlist to watch (legacy/default)
    pub playlist_id: String,
    /// Optional: after processing, move videos to this playlist
    pub processed_playlist_id: Option<String>,
    /// Multiple playlists with specific patterns and SP settings
    #[serde(default)]
    pub playlists: Vec<PlaylistPatternConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistPatternConfig {
    pub id: String,
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Enable SP tasks for this playlist (overrides global sp_enabled)
    pub sp_enabled: Option<bool>,
    /// SP project ID for this playlist (overrides global sp_project_id)
    pub sp_project_id: Option<String>,
    /// Target Obsidian folder for this playlist (e.g. "Proyectos/Rust")
    pub obsidian_folder: Option<String>,
    /// Target Obsidian vault path for this playlist (overrides global obsidian_vault)
    pub obsidian_vault: Option<String>,
    /// Max transcript characters for this playlist (overrides global max_transcript_chars)
    pub max_transcript_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingConfig {
    /// LLM base URL (OpenAI-compatible, e.g. http://192.168.1.150:8080 for llama.cpp)
    pub llm_base_url: Option<String>,
    /// Model name (e.g. model filename for llama.cpp)
    pub llm_model: Option<String>,
    /// Optional API key (llama.cpp doesn't need one)
    pub llm_api_key: Option<String>,
    /// Max transcript characters to send to LLM (default: 25000)
    pub max_transcript_chars: Option<usize>,
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

/// Local storage configuration for downloaded videos
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Directory to store downloaded videos (default: ~/Videos/yt2action)
    pub videos_dir: Option<String>,
    /// Video quality for yt-dlp (e.g. "worst", "best", "best[height<=1080]")
    /// Default: "worst" since we only need audio for transcription
    pub quality: Option<String>,
    /// Path to yt-dlp binary
    pub yt_dlp_path: Option<String>,
    /// Whether to keep subtitle files alongside the video
    pub keep_subtitles: Option<bool>,
    /// Preferred subtitle languages (comma-separated, e.g. "en,es,es-419")
    pub subtitle_langs: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            videos_dir: None,
            quality: Some("worst".to_string()),
            yt_dlp_path: None,
            keep_subtitles: Some(true),
            subtitle_langs: Some("en,es,es-419,en-US".to_string()),
        }
    }
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

        // Resolve credentials path relative to config file location if relative
        if let Some(ref creds) = cfg.youtube.credentials_path
            && !PathBuf::from(creds).is_absolute()
            && let Some(config_dir) = path.parent()
        {
            let resolved = config_dir.join(creds);
            if resolved.exists() {
                cfg.youtube.credentials_path =
                    Some(resolved.to_string_lossy().to_string());
            }
        }

        Ok(cfg)
    }

    /// Get resolved videos directory
    pub fn videos_dir(&self) -> Result<PathBuf> {
        let dir = self
            .storage
            .videos_dir
            .clone()
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                format!("{}/Videos/yt2action", home)
            });
        let path = PathBuf::from(dir);
        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create videos dir: {:?}", path))?;
        Ok(path)
    }

    /// Get path to yt-dlp binary
    pub fn yt_dlp_path(&self) -> String {
        self.storage
            .yt_dlp_path
            .clone()
            .unwrap_or_else(|| "yt-dlp".to_string())
    }

    /// Get video quality
    pub fn video_quality(&self) -> &str {
        self.storage
            .quality
            .as_deref()
            .unwrap_or("worst")
    }

    /// Default config path: ~/.config/yt2action/config.toml
    pub fn default_path() -> Result<PathBuf> {
        let dir = directories::ProjectDirs::from("com", "leofishman", "yt2action")
            .context("Failed to find config directory")?;
        Ok(dir.config_dir().join("config.toml"))
    }
}
