mod config;
mod downloader;
mod obsidian;
mod processor;
mod sp_api;
mod transcript;
mod types;
mod youtube;

use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
use types::ProcessResult;
use youtube::YoutubeClient;
use anyhow::Context;

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

        /// Only process N videos (default: 5)
        #[arg(short, long, default_value = "5")]
        limit: usize,

        /// Reprocess videos even if already processed
        #[arg(short, long)]
        force: bool,

        /// Apply a Fabric pattern for rich content analysis
        /// (e.g. "extract_wisdom", "summarize", "analyze_claims")
        /// Patterns stored in ~/.config/fabric/patterns/
        #[arg(short, long)]
        pattern: Option<String>,
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
        Commands::Run {
            config,
            limit,
            force,
            pattern,
        } => cmd_run(config, limit, force, pattern).await,
        Commands::Init { output } => cmd_init(output),
        Commands::List { config } => cmd_list(config).await,
        Commands::Auth { credentials } => cmd_auth(credentials).await,
        Commands::Health => cmd_health().await,
    }
}

async fn cmd_run(
    config_path: Option<PathBuf>,
    limit: usize,
    force: bool,
    pattern: Option<String>,
) -> anyhow::Result<()> {
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

    // Filter videos: if force, ignore state; otherwise skip processed
    let to_process: Vec<_> = if force {
        videos.iter().take(limit).collect()
    } else {
        videos
            .iter()
            .filter(|v| !state.is_processed(&v.id))
            .take(limit)
            .collect()
    };

    if to_process.is_empty() {
        log::info!("No new videos to process.");
        if videos.len() > 0 && !force {
            log::info!("Tip: use --force to reprocess already processed videos.");
        }
        return Ok(());
    }

    log::info!(
        "Processing {} video{}...",
        to_process.len(),
        if to_process.len() == 1 { "" } else { "s" }
    );

    // Build valid folder list for the AI
    let mut valid_folders = vec![
        "Learn".into(),
        "Ideas".into(),
        "Resources".into(),
        "Health".into(),
        "Things".into(),
        "Resources/YouTube".into(),
    ];
    for folder in cfg.output.category_folders.values() {
        if !valid_folders.contains(folder) {
            valid_folders.push(folder.clone());
        }
    }

    let proc = processor::Processor::new(
        cfg.processing
            .llm_base_url
            .as_deref()
            .context("llm_base_url not set in config")?,
        cfg.processing
            .llm_model
            .as_deref()
            .context("llm_model not set in config")?,
        cfg.processing
            .llm_api_key
            .as_deref()
            .unwrap_or(""),
        valid_folders,
    );

    // Resolve yt-dlp path and storage config
    let yt_dlp_path = cfg.yt_dlp_path();
    let videos_dir = cfg.videos_dir()?;
    let quality = cfg.video_quality().to_string();
    let subtitle_langs: Vec<&str> = cfg
        .storage
        .subtitle_langs
        .as_deref()
        .unwrap_or("en,es,es-419,en-US")
        .split(',')
        .map(|s| s.trim())
        .collect();

    // Track results for the final report
    let mut results: Vec<ProcessResult> = Vec::new();

    for video in &to_process {
        log::info!("Processing: {}", video.title);

        // Step 1: Download video + subtitles locally via yt-dlp
        log::info!("  📥 Downloading video {}...", video.id);
        let transcript_result = transcript::get_transcript(
            &yt_dlp_path,
            &video.id,
            &videos_dir,
            &quality,
            &subtitle_langs,
        )
        .await?;

        // Step 2: AI processing
        match proc.process(video, transcript_result.text.as_deref()).await {
            Ok(mut processed) => {
                // Set local video path
                processed.local_video_path = transcript_result
                    .video_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string());

                // If a Fabric pattern is specified, run the second phase
                if let Some(ref pattern_name) = pattern {
                    let patterns_dir = format!(
                        "{}/.config/fabric/patterns",
                        std::env::var("HOME").unwrap_or_default()
                    );

                    match proc
                        .process_with_pattern(
                            video,
                            transcript_result.text.as_deref(),
                            pattern_name,
                            &patterns_dir,
                        )
                        .await
                    {
                        Ok(fabric_output) => {
                            processed.fabric_output = Some(fabric_output);
                            log::info!("  📜 Fabric pattern output stored");
                        }
                        Err(e) => {
                            log::warn!("  ⚠️ Fabric pattern failed: {e}");
                        }
                    }
                }
                let mut result = ProcessResult {
                    video_title: video.title.clone(),
                    video_url: format!("https://youtube.com/watch?v={}", video.id),
                    target_folder: processed.target_folder.clone(),
                    note_path: None,
                    sp_task_id: None,
                    moved_to_processed: false,
                    suggested_action: processed.classification.suggested_action.clone(),
                    tags: processed.classification.tags.clone(),
                    error: None,
                };

                // Create Obsidian note (primary output)
                if let Some(ref vault) = cfg.output.obsidian_vault {
                    let vault_path = PathBuf::from(vault);
                    match obsidian::create_note(&vault_path, &processed) {
                        Ok(path) => {
                            result.note_path = Some(path.to_string_lossy().to_string());
                            log::info!("  📝 Note created: {:?}", path);
                        }
                        Err(e) => {
                            log::error!("  ❌ Failed to create note: {e}");
                        }
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
                        Ok(id) => {
                            result.sp_task_id = Some(id.clone());
                            log::info!("  ✅ Task created: {id}");
                        }
                        Err(e) => log::error!("  ❌ Failed to create task: {e}"),
                    }
                }

                // Post-processing: move video to processed playlist
                if let Some(ref processed_pl) = cfg.youtube.processed_playlist_id {
                    match yt
                        .add_to_playlist(processed_pl, &video.id)
                        .await
                    {
                        Ok(_new_item_id) => {
                            // Remove from source playlist
                            match yt.remove_from_playlist(&video.playlist_item_id).await {
                                Ok(()) => {
                                    result.moved_to_processed = true;
                                    log::info!(
                                        "  ✅ Moved to processed playlist: {processed_pl}"
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "  ⚠️ Added to processed playlist but could not \
                                         remove from source: {e}"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "  ⚠️ Could not add to processed playlist: {e}"
                            );
                        }
                    }
                }

                // Mark as processed (only if not moving — if moving, it's already gone)
                if cfg.youtube.processed_playlist_id.is_none() {
                    state.mark_processed(video.id.clone());
                }

                results.push(result);
            }
            Err(e) => {
                log::error!("  ❌ Failed to process '{}': {e}", video.title);
                results.push(ProcessResult {
                    video_title: video.title.clone(),
                    video_url: format!("https://youtube.com/watch?v={}", video.id),
                    target_folder: String::new(),
                    note_path: None,
                    sp_task_id: None,
                    moved_to_processed: false,
                    suggested_action: types::SuggestedAction::SaveForLater,
                    tags: vec![],
                    error: Some(e.to_string()),
                });
            }
        }
    }

    // Save state (only for videos not moved to another playlist)
    save_state(&state_path, &state)?;

    // --- FINAL REPORT ---
    print_report(&results, &cfg);

    Ok(())
}

/// Print a beautiful summary report after processing
fn print_report(results: &[ProcessResult], cfg: &Config) {
    let _total = results.len();
    let errors: Vec<_> = results.iter().filter(|r| r.error.is_some()).collect();
    let success: Vec<_> = results.iter().filter(|r| r.error.is_none()).collect();

    println!();
    println!("══════════════════════════════════════════════════════");
    println!("           📋 REPORTE DE PROCESAMIENTO");
    println!("══════════════════════════════════════════════════════");
    println!();

    for (i, r) in results.iter().enumerate() {
        let icon = if r.error.is_some() { "❌" } else { "✅" };
        println!("  {icon}  {}. {}", i + 1, r.video_title);
        println!("     🔗  {0}", r.video_url);

        if let Some(ref path) = r.note_path {
            println!("     📝  {path}");
        }
        if let Some(ref task_id) = r.sp_task_id {
            println!("     📋  SP task: {task_id}");
        }
        if r.moved_to_processed {
            println!("     📁  Movido a playlist de procesados");
        }
        if let Some(ref action) = action_emoji(&r.suggested_action) {
            println!("     {action}");
        }
        if !r.tags.is_empty() {
            println!("     🏷️  {}", r.tags.join(", "));
        }
        if let Some(ref err) = r.error {
            println!("     ❌  Error: {err}");
        }
        println!();
    }

    println!("──────────────────────────────────────────────────");
    println!(
        "  Total: {} procesado{} | {} error{}",
        success.len(),
        if success.len() == 1 { "" } else { "s" },
        errors.len(),
        if errors.len() == 1 { "" } else { "es" }
    );
    if let Some(ref vault) = cfg.output.obsidian_vault {
        println!("  📁  Vault: {vault}");
    }
    if let Some(ref processed_pl) = cfg.youtube.processed_playlist_id {
        println!("  📁  Playlist destino: {processed_pl}");
    }
    println!("══════════════════════════════════════════════════════");
    println!();
}

fn action_emoji(action: &types::SuggestedAction) -> Option<&'static str> {
    match action {
        types::SuggestedAction::WatchFull => {
            Some("👀  Sugerencia: Ver completo (vale la pena)")
        }
        types::SuggestedAction::ReadTranscript => {
            Some("📖  Sugerencia: Solo leer resumen")
        }
        types::SuggestedAction::SaveForLater => {
            Some("💾  Sugerencia: Guardar para después")
        }
        types::SuggestedAction::Archive => {
            Some("📦  Sugerencia: Archivar (referencia)")
        }
    }
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

# Optional: after processing, move videos here (creates a second private playlist)
# processed_playlist_id = "YOUR_PROCESSED_PLAYLIST_ID"

[processing]
# LLM base URL (OpenAI-compatible)
# Para llama.cpp: http://192.168.1.150:8080
# Para OpenAI: https://api.openai.com/v1
llm_base_url = "http://localhost:11434"
llm_model = "llama3.2"

# API key (opcional — llama.cpp no necesita)
# llm_api_key = "sk-..."

[storage]
# Directorio para almacenar videos descargados localmente
# Se crea automáticamente si no existe
# videos_dir = "~/Videos/yt2action"

# Calidad del video (por defecto "worst" — solo necesitamos audio para transcripción)
# quality = "worst"

# Ruta al binario yt-dlp (si no está en PATH)
# yt_dlp_path = "yt-dlp"

# Idiomas de subtítulos preferidos
# subtitle_langs = "en,es,es-419,en-US"

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
