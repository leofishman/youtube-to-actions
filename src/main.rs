mod config;
mod obsidian;
mod processor;
mod sp_api;
mod transcript;
mod types;
mod youtube;

use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
use youtube::YoutubeClient;

#[derive(Parser)]
#[command(name = "yt2action", about = "YouTube playlist → tasks/notes automator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the full pipeline: fetch playlist, process videos, create tasks
    Run {
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Only process N newest videos
        #[arg(short, long, default_value = "5")]
        limit: usize,
    },
    /// Initialize a default config file
    Init {
        /// Output path for the config file
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// List videos from the playlist without processing
    List {
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Test YouTube OAuth connection
    Auth {
        /// Path to credentials.json
        #[arg(short, long)]
        credentials: Option<PathBuf>,
    },
    /// Health check: test SP API
    Health,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run { config, limit } => cmd_run(config, limit).await,
        Commands::Init { output } => cmd_init(output),
        Commands::List { config } => cmd_list(config).await,
        Commands::Auth { credentials } => cmd_auth(credentials).await,
        Commands::Health => cmd_health().await,
    }
}

async fn cmd_run(config_path: Option<PathBuf>, limit: usize) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;

    // Check SP is running
    let sp = sp_api::SpClient::new();
    if cfg.output.sp_enabled {
        match sp.health_check().await {
            Ok(true) => log::info!("Super Productivity API: connected"),
            _ => log::warn!("Super Productivity not running. Tasks will be skipped."),
        }
    }

    // Authenticate with YouTube
    let creds_path = cfg
        .youtube
        .credentials_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("credentials.json"));

    log::info!("Authenticating with Google...");
    let yt = YoutubeClient::authenticate(&creds_path).await?;
    log::info!("YouTube: authenticated.");

    // Fetch playlist
    log::info!("Fetching playlist {}...", cfg.youtube.playlist_id);
    let videos = yt.get_playlist_videos(&cfg.youtube.playlist_id).await?;
    log::info!("Found {} videos in playlist", videos.len());

    // Load state
    let state_path = get_state_path()?;
    let mut state = load_state(&state_path);

    // Filter new videos
    let new_videos: Vec<_> = videos
        .iter()
        .filter(|v| !state.is_processed(&v.id))
        .take(limit)
        .collect();

    if new_videos.is_empty() {
        log::info!("No new videos to process.");
        return Ok(());
    }

    log::info!("Processing {} new videos...", new_videos.len());

    // Initialize processor with available vault folders for AI to choose from
    let mut valid_folders = vec![
        "Learn".into(),
        "Ideas".into(),
        "Resources".into(),
        "Health".into(),
        "Things".into(),
        "Resources/YouTube".into(),
    ];
    // Add any user-configured overrides
    for folder in cfg.output.category_folders.values() {
        if !valid_folders.contains(folder) {
            valid_folders.push(folder.clone());
        }
    }

    let proc = processor::Processor::new(
        cfg.processing
            .ollama_url
            .as_deref()
            .unwrap_or("http://localhost:11434"),
        cfg.processing
            .ollama_model
            .as_deref()
            .unwrap_or("llama3.2"),
        valid_folders,
    );

    for video in &new_videos {
        log::info!("Processing: {}", video.title);

        // Get transcript
        let transcript = transcript::get_transcript(&video.id).await?;

        // AI processing
        match proc.process(video, transcript.as_deref()).await {
            Ok(processed) => {
                // Create Obsidian note (primary output)
                if let Some(ref vault) = cfg.output.obsidian_vault {
                    let vault_path = PathBuf::from(vault);
                    match obsidian::create_note(&vault_path, &processed) {
                        Ok(path) => log::info!("  📝 Note created: {:?}", path),
                        Err(e) => log::error!("  ❌ Failed to create note: {e}"),
                    }
                }

                // Create SP task (optional)
                if cfg.output.sp_enabled {
                    let title = format!("[📺] {}", processed.video.title);
                    let notes = format!(
                        "{}\n\n## Puntos clave\n{}\n\n🔗 https://youtube.com/watch?v={}",
                        processed.summary,
                        processed
                            .key_points
                            .iter()
                            .map(|p| format!("- {p}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        processed.video.id
                    );

                    let task = types::SpTask {
                        title,
                        notes,
                        project_id: cfg
                            .output
                            .sp_project_id
                            .clone()
                            .unwrap_or_else(|| "INBOX_PROJECT".to_string()),
                        tag_ids: processed.classification.tags.clone(),
                    };

                    match sp.create_task(&task).await {
                        Ok(id) => log::info!("  ✅ Task created: {id}"),
                        Err(e) => log::error!("  ❌ Failed to create task: {e}"),
                    }
                }

                // Mark as processed
                state.mark_processed(video.id.clone());
            }
            Err(e) => {
                log::error!("  ❌ Failed to process '{}': {e}", video.title);
            }
        }
    }

    // Save state
    save_state(&state_path, &state)?;
    log::info!("Done! Processed {} videos.", new_videos.len());

    Ok(())
}

async fn cmd_list(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;

    let creds_path = cfg
        .youtube
        .credentials_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("credentials.json"));

    let yt = YoutubeClient::authenticate(&creds_path).await?;
    let videos = yt.get_playlist_videos(&cfg.youtube.playlist_id).await?;

    println!("Playlist: {}", cfg.youtube.playlist_id);
    println!("Total videos: {}\n", videos.len());

    for (i, v) in videos.iter().enumerate() {
        let duration = match v.duration_seconds {
            Some(s) if s > 3600 => format!("{:.1}h", s as f64 / 3600.0),
            Some(s) if s > 60 => format!("{}m", s / 60),
            Some(s) => format!("{}s", s),
            None => "?".to_string(),
        };
        println!("{}. [{}] {} — {}", i + 1, duration, v.title, v.channel);
    }

    Ok(())
}

async fn cmd_auth(credentials: Option<PathBuf>) -> anyhow::Result<()> {
    let path = credentials.unwrap_or_else(|| PathBuf::from("credentials.json"));

    if !path.exists() {
        anyhow::bail!(
            "credentials.json not found at {:?}\n\
             Download it from Google Cloud Console > APIs & Services > Credentials\n\
             (OAuth 2.0 Client ID > Desktop App)",
            path
        );
    }

    println!("🔑 Authenticating with Google...");
    println!("   A browser window will open. Sign in with your Google account.\n");

    let yt = YoutubeClient::authenticate(&path).await?;

    // Test with a simple API call
    match yt.get_playlist_videos("PLEASE_IGNORE_THIS").await {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            // Expected: invalid playlist ID, but that means auth worked
            if msg.contains("notFound") || msg.contains("404") || msg.contains("invalid") {
                println!("   ✅ Auth works! (Got expected error about invalid playlist)");
            } else {
                println!("   ❌ Auth failed: {e}");
                return Err(e);
            }
        }
    }

    println!("\n✅ Authentication successful!");
    println!("   Token cached in token_cache.json (chmod 600 recommended)");
    Ok(())
}

async fn cmd_health() -> anyhow::Result<()> {
    println!("🔍 Health Check\n");

    // Super Productivity
    println!("📋 Super Productivity...");
    let sp = sp_api::SpClient::new();
    match sp.health_check().await {
        Ok(true) => println!("   ✅ API responding (port 3876)."),
        Ok(false) => println!("   ❌ API not ready."),
        Err(e) => println!("   ❌ Not running: {e}"),
    }

    Ok(())
}

fn cmd_init(output: Option<PathBuf>) -> anyhow::Result<()> {
    let path = match output {
        Some(p) => p,
        None => Config::default_path()?,
    };

    if path.exists() {
        anyhow::bail!("Config file already exists: {:?}", path);
    }

    let default_config = r#"# yt2action Configuration
# Run `yt2action init` to create this file, then fill in your values.

[youtube]
# Path to credentials.json (Google OAuth Desktop App credentials)
# Download from: Google Cloud Console > APIs & Services > Credentials
# Create > OAuth client ID > Desktop application
credentials_path = "credentials.json"

# Private playlist ID to watch
# From the URL: https://www.youtube.com/playlist?list=PLAYLIST_ID
playlist_id = "YOUR_PLAYLIST_ID"

[processing]
# AI processing via Ollama (local)
ollama_url = "http://localhost:11434"
ollama_model = "llama3.2"

# Alternative: remote API (OpenAI-compatible), uncomment to use instead:
# api_url = "https://api.openai.com/v1"
# api_key = "sk-..."
# api_model = "gpt-4o-mini"

[output]
# Obsidian vault — notes go to Learn/, Ideas/, Resources/ etc. (automatically)
obsidian_vault = "/home/leo/Memory/lenovo1"

# Super Productivity (optional — enable if you also want tasks in SP)
sp_enabled = false
sp_project_id = "INBOX_PROJECT"
sp_tag_learn = "learn"
sp_tag_dev = "dev"

# Category → Vault folder mapping (optional — uncomment to override defaults)
# [output.category_folders]
# tutorial = "Knowledge/Tutorials"
# concept = "Knowledge/Concepts"
# health = "Wellness"
"#;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, default_config)?;
    println!("✅ Config file created: {:?}", path);
    println!("   Edit it with your playlist ID and verify credentials.json exists.");

    Ok(())
}

fn load_config(path: Option<PathBuf>) -> anyhow::Result<Config> {
    let path = match path {
        Some(p) => p,
        None => Config::default_path()?,
    };

    if !path.exists() {
        anyhow::bail!(
            "Config file not found: {:?}\nRun `yt2action init` to create one.",
            path
        );
    }

    Config::from_file(&path)
}

fn get_state_path() -> anyhow::Result<PathBuf> {
    let dir = directories::ProjectDirs::from("com", "leofishman", "yt2action")
        .ok_or_else(|| anyhow::anyhow!("Failed to find data directory"))?;
    let path = dir.data_dir().join("state.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

fn load_state(path: &PathBuf) -> types::ProcessState {
    if !path.exists() {
        return types::ProcessState::new();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => types::ProcessState::new(),
    }
}

fn save_state(path: &PathBuf, state: &types::ProcessState) -> anyhow::Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    std::fs::write(path, &content)?;
    Ok(())
}
