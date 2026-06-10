mod config;
mod downloader;
mod obsidian;
mod processor;
mod sp_api;
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

        /// Apply Fabric patterns for rich content analysis (comma-separated or multiple flags, e.g. "summarize,extract_wisdom")
        #[arg(short, long, value_delimiter = ',', num_args = 1..)]
        patterns: Vec<String>,

        /// Actually download the video file locally (default: false, only fetches transcript)
        #[arg(long)]
        download_video: bool,

        /// Dry run simulation: fetch and process with LLM, but do not write notes, tasks, or alter state
        #[arg(long)]
        dry_run: bool,
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
    /// Process a single video by ID (or by position in playlist)
    Process {
        /// Video ID or position number (1-based) in the playlist
        video: String,

        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// Use Fabric patterns for analysis (comma-separated or multiple flags, e.g. "summarize,extract_wisdom")
        #[arg(short, long, value_delimiter = ',', num_args = 1..)]
        patterns: Vec<String>,

        /// Actually download the video file locally (default: false, only fetches transcript)
        #[arg(long)]
        download_video: bool,

        /// Dry run simulation: fetch and process with LLM, but do not write notes, tasks, or alter state
        #[arg(long)]
        dry_run: bool,
    },
    /// Synchronize Fabric patterns from the official repository to ~/.config/fabric/patterns/
    SyncPatterns {
        /// Force update and overwrite existing patterns
        #[arg(short, long)]
        force: bool,
    },
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
            patterns,
            download_video,
            dry_run,
        } => cmd_run(config, limit, force, patterns, download_video, dry_run).await,
        Commands::Init { output } => cmd_init(output),
        Commands::List { config } => cmd_list(config).await,
        Commands::Auth { credentials } => cmd_auth(credentials).await,
        Commands::Health => cmd_health().await,
        Commands::Process { video, config, patterns, download_video, dry_run } => {
            cmd_process(video, config, patterns, download_video, dry_run).await
        }
        Commands::SyncPatterns { force } => cmd_sync_patterns(force).await,
    }
}

async fn cmd_run(
    config_path: Option<PathBuf>,
    limit: usize,
    force: bool,
    patterns: Vec<String>,
    download_video_flag: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let start_time = std::time::Instant::now();
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

    // Collect playlists to process with their SP settings and overrides
    let playlists_to_process: Vec<_> = {
        let mut playlists = Vec::new();
        
        // Add legacy default playlist if no other playlists are configured
        if cfg.youtube.playlists.is_empty() {
            playlists.push((
                cfg.youtube.playlist_id.clone(),
                vec![],
                None, // sp_enabled - use global
                cfg.output.sp_project_id.clone(), // sp_project_id - use global
                None, // obsidian_folder - use default
                None, // obsidian_vault - use default
                None, // max_transcript_chars - use global
            ));
        }
        
        // Add configured playlists
        for p in &cfg.youtube.playlists {
            playlists.push((
                p.id.clone(),
                p.patterns.clone(),
                p.sp_enabled,
                p.sp_project_id.clone(),
                p.obsidian_folder.clone(),
                p.obsidian_vault.clone(),
                p.max_transcript_chars,
            ));
        }
        
        playlists
    };

    if playlists_to_process.is_empty() {
        log::error!("No playlists configured to process.");
        return Err(anyhow::anyhow!("No playlists configured"));
    }

    // Track results for the final report
    let mut results: Vec<ProcessResult> = Vec::new();

    // Process each playlist
    let mut total_processed = 0;
    for (playlist_id, playlist_patterns, playlist_sp_enabled, playlist_project_id, playlist_obsidian_folder, playlist_obsidian_vault, playlist_max_transcript_chars) in playlists_to_process {
        if total_processed >= limit {
            break;
        }

        log::info!("Processing playlist: {}", playlist_id);
        
        // Fetch videos for this playlist
        let videos = match yt.get_playlist_videos(&playlist_id).await {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to fetch playlist {}: {}", playlist_id, e);
                continue;
            }
        };

        // Filter videos: if force, ignore state; otherwise skip processed
        let to_process: Vec<_> = if force {
            videos.iter().take(limit - total_processed).collect()
        } else {
            videos
                .iter()
                .filter(|v| !state.is_processed(&v.id))
                .take(limit - total_processed)
                .collect()
        };

        if to_process.is_empty() {
            log::info!("No new videos in playlist {}. Continuing...", playlist_id);
            continue;
        }

        log::info!(
            "Processing {} video{} (playlist: {})...",
            to_process.len(),
            if to_process.len() == 1 { "" } else { "s" },
            playlist_id
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
        playlist_max_transcript_chars.or(cfg.processing.max_transcript_chars),
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



    for video in &to_process {
        log::info!("Processing: {}", video.title);

        // Step 1: Download video + subtitles locally via yt-dlp
        if download_video_flag {
            log::info!("  📥 Downloading video {}...", video.id);
        } else {
            log::info!("  📥 Fetching transcript for {}...", video.id);
        }
        let transcript_result = downloader::download_video(
            &yt_dlp_path,
            &video.id,
            &videos_dir,
            &quality,
            &subtitle_langs,
            download_video_flag,
        )
        .await?;

        // Step 2: AI processing
        match proc.process(video, transcript_result.transcript.as_deref()).await {
            Ok(mut processed) => {
                // Set local video path
                processed.local_video_path = transcript_result
                    .video_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string());

                // If Fabric patterns are specified, run the second phase
                let patterns_to_run = if !patterns.is_empty() {
                    patterns.clone()
                } else {
                    playlist_patterns.clone()
                };

                let mut pattern_execution_times = Vec::new();

                for pattern_name in &patterns_to_run {
                    let patterns_dir = format!(
                        "{}/.config/fabric/patterns",
                        std::env::var("HOME").unwrap_or_default()
                    );
                    log::info!("  🧠 Ejecutando patrón de Fabric: {}...", pattern_name);

                    let pattern_start = std::time::Instant::now();
                    let pattern_res = proc
                        .process_with_pattern(
                            video,
                            transcript_result.transcript.as_deref(),
                            pattern_name,
                            &patterns_dir,
                        )
                        .await;

                    let duration_str = format!("{:.1}s", pattern_start.elapsed().as_secs_f64());
                    pattern_execution_times.push((pattern_name.clone(), duration_str));

                    match pattern_res {
                        Ok(fabric_output) => {
                            if processed.fabric_output.is_none() {
                                processed.fabric_output = Some(fabric_output.clone());
                            } else {
                                let mut current = processed.fabric_output.take().unwrap();
                                current.push_str(&format!("\n\n---\n\n# Patrón: {}\n\n{}", pattern_name, fabric_output));
                                processed.fabric_output = Some(current);
                            }
                            log::info!("  📜 Fabric pattern output stored");
                            
                            // Log temporal de salidas de Fabric
                            if let Err(e) = log_fabric_output_temp(video, pattern_name, &fabric_output) {
                                log::warn!("  ⚠️ No se pudo guardar el log temporal de Fabric: {e}");
                            }
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
                    pattern_execution_times,
                };

                // Create Obsidian note (primary output)
                let active_vault = playlist_obsidian_vault.as_ref().or(cfg.output.obsidian_vault.as_ref());
                if let Some(ref vault) = active_vault {
                    let vault_path = PathBuf::from(vault);
                    match obsidian::create_note(&vault_path, &processed, playlist_obsidian_folder.as_deref(), dry_run) {
                        Ok(path) => {
                            result.note_path = Some(path.to_string_lossy().to_string());
                            if dry_run {
                                log::info!("  📝 [Simulado] Note would be created at: {:?}", path);
                            } else {
                                log::info!("  📝 Note created: {:?}", path);
                            }
                        }
                        Err(e) => {
                            log::error!("  ❌ Failed to create note: {e}");
                        }
                    }
                }

                // Create SP task (optional)
                let sp_enabled = playlist_sp_enabled.unwrap_or(cfg.output.sp_enabled);
                let project_id_raw = playlist_project_id
                    .clone()
                    .or_else(|| cfg.output.sp_project_id.clone())
                    .unwrap_or_else(|| "INBOX_PROJECT".to_string());
                
                if sp_enabled {
                    let resolved_id = match sp.resolve_project_id(&project_id_raw).await {
                        Ok(Some(id)) => {
                            log::debug!("Resolved SP project '{}' to ID: {}", project_id_raw, id);
                            id
                        }
                        _ => {
                            log::debug!("Could not resolve SP project '{}', using raw value", project_id_raw);
                            project_id_raw
                        }
                    };

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
                        project_id: resolved_id,
                        tag_ids: processed.classification.tags.clone(),
                    };

                    if dry_run {
                        log::info!("  ✅ [Simulado] SP task would be created: {}", task.title);
                        result.sp_task_id = Some("SIMULATED_TASK_ID".to_string());
                    } else {
                        match sp.create_task(&task).await {
                            Ok(id) => {
                                result.sp_task_id = Some(id.clone());
                                log::info!("  ✅ Task created: {id}");
                            }
                            Err(e) => log::error!("  ❌ Failed to create task: {e}"),
                        }
                    }
                }

                // Post-processing: move video to processed playlist
                if let Some(ref processed_pl) = cfg.youtube.processed_playlist_id {
                    if dry_run {
                        result.moved_to_processed = true;
                        log::info!("  ✅ [Simulado] Video would be moved to processed playlist: {processed_pl}");
                    } else {
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
                }

                // Mark as processed (only if not moving — if moving, it's already gone)
                if cfg.youtube.processed_playlist_id.is_none() {
                    if dry_run {
                        log::info!("  💾 [Simulado] Video would be marked as processed in local state");
                    } else {
                        state.mark_processed(video.id.clone());
                    }
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
                    pattern_execution_times: vec![],
                });
            }
        }
    }

        total_processed += to_process.len();
    }

    // Save state (only for videos not moved to another playlist)
    if !dry_run {
        save_state(&state_path, &state)?;
    } else {
        log::info!("💾 [Simulado] State changes would be saved to {:?}", state_path);
    }

    // --- FINAL REPORT ---
    print_report(&results, &cfg, start_time.elapsed(), dry_run);

    Ok(())
}

/// Print a beautiful summary report after processing
fn print_report(results: &[ProcessResult], cfg: &Config, total_duration: std::time::Duration, dry_run: bool) {
    let _total = results.len();
    let errors: Vec<_> = results.iter().filter(|r| r.error.is_some()).collect();
    let success: Vec<_> = results.iter().filter(|r| r.error.is_none()).collect();

    println!();
    println!("══════════════════════════════════════════════════════");
    if dry_run {
        println!("       📋 REPORTE DE PROCESAMIENTO (MODO SIMULACIÓN)");
    } else {
        println!("           📋 REPORTE DE PROCESAMIENTO");
    }
    println!("══════════════════════════════════════════════════════");
    println!();

    for (i, r) in results.iter().enumerate() {
        let icon = if r.error.is_some() { "❌" } else { "✅" };
        println!("  {icon}  {}. {}", i + 1, r.video_title);
        println!("     🔗  {0}", r.video_url);

        if let Some(ref path) = r.note_path {
            if dry_run {
                println!("     📝  [Simulado] {path}");
            } else {
                println!("     📝  {path}");
            }
        }
        if let Some(ref task_id) = r.sp_task_id {
            if dry_run {
                println!("     📋  [Simulado] SP task: {task_id}");
            } else {
                println!("     📋  SP task: {task_id}");
            }
        }
        if r.moved_to_processed {
            if dry_run {
                println!("     📁  [Simulado] Movido a playlist de procesados");
            } else {
                println!("     📁  Movido a playlist de procesados");
            }
        }
        if let Some(ref action) = action_emoji(&r.suggested_action) {
            println!("     {action}");
        }
        if !r.tags.is_empty() {
            println!("     🏷️  {}", r.tags.join(", "));
        }
        if !r.pattern_execution_times.is_empty() {
            println!("     🧠  Tiempos de patrones:");
            for (p_name, duration) in &r.pattern_execution_times {
                println!("         - {}: {}", p_name, duration);
            }
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
    let mins = total_duration.as_secs() / 60;
    let secs = total_duration.as_secs() % 60;
    let duration_str = if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{:.1}s", total_duration.as_secs_f64())
    };
    println!("  ⏱️   Tiempo total de ejecución: {duration_str}");
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

/// Process a single video by ID, URL, or position in playlist
async fn cmd_process(
    video_arg: String,
    config_path: Option<PathBuf>,
    patterns: Vec<String>,
    download_video_flag: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;

    let resolved_id = extract_video_id(&video_arg);

    // Resolve video: check if position number first
    let video = if resolved_id.parse::<usize>().is_ok() {
        let pos = resolved_id.parse::<usize>().unwrap();

        // Authenticate with YouTube (only required for playlist pos resolution)
        let creds_path = cfg
            .youtube
            .credentials_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("credentials.json"));

        log::info!("Authenticating with Google OAuth (required for playlist pos)...");
        let yt = YoutubeClient::authenticate(&creds_path).await?;
        log::info!("YouTube: authenticated.");

        log::info!("Fetching playlist {} to resolve position...", cfg.youtube.playlist_id);
        let playlist_videos = yt.get_playlist_videos(&cfg.youtube.playlist_id).await?;
        if pos > 0 && pos <= playlist_videos.len() {
            playlist_videos[pos - 1].clone()
        } else {
            anyhow::bail!(
                "Position {} out of range (playlist has {} videos)",
                pos,
                playlist_videos.len()
            );
        }
    } else {
        log::info!("Fetching public video details via yt-dlp for ID: {}...", resolved_id);
        get_video_metadata_ytdlp(&cfg.yt_dlp_path(), &resolved_id)?
    };

    println!("══════════════════════════════════════════════════════");
    println!("  📺 PROCESANDO VIDEO INDIVIDUAL");
    println!("══════════════════════════════════════════════════════");
    println!("  Título: {}", video.title);
    println!("  ID: {}", video.id);
    println!("  Canal: {}", video.channel);
    println!();

    // Load state
    let state_path = get_state_path()?;
    let mut state = load_state(&state_path);

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
        cfg.processing.max_transcript_chars,
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

    // Download video + subtitles
    if download_video_flag {
        log::info!("  📥 Descargando video {}...", video.id);
    } else {
        log::info!("  📥 Obteniendo transcripción para {}...", video.id);
    }
    let transcript_result = downloader::download_video(
        &yt_dlp_path,
        &video.id,
        &videos_dir,
        &quality,
        &subtitle_langs,
        download_video_flag,
    )
    .await?;

  // AI processing
        match proc.process(&video, transcript_result.transcript.as_deref()).await {
            Ok(mut processed) => {
                // Set local video path
                processed.local_video_path = transcript_result
                    .video_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string());

            // If Fabric patterns are specified, run the second phase
            for pattern_name in &patterns {
                let patterns_dir = format!(
                    "{}/.config/fabric/patterns",
                    std::env::var("HOME").unwrap_or_default()
                );
                log::info!("  🧠 Ejecutando patrón de Fabric: {}...", pattern_name);

                match proc
                    .process_with_pattern(
                        &video,
                        transcript_result.transcript.as_deref(),
                        pattern_name,
                        &patterns_dir,
                    )
                    .await
                {
                    Ok(fabric_output) => {
                        if processed.fabric_output.is_none() {
                            processed.fabric_output = Some(fabric_output.clone());
                        } else {
                            let mut current = processed.fabric_output.take().unwrap();
                            current.push_str(&format!("\n\n---\n\n# Patrón: {}\n\n{}", pattern_name, fabric_output));
                            processed.fabric_output = Some(current);
                        }
                        log::info!("  📜 Fabric pattern output stored");
                        
                        // Log temporal de salidas de Fabric
                        if let Err(e) = log_fabric_output_temp(&video, pattern_name, &fabric_output) {
                            log::warn!("  ⚠️ No se pudo guardar el log temporal de Fabric: {e}");
                        }
                    }
                    Err(e) => {
                        log::warn!("  ⚠️ Fabric pattern failed: {e}");
                    }
                }
            }

            // Create Obsidian note
            let mut note_path = None;
            if let Some(ref vault) = cfg.output.obsidian_vault {
                let vault_path = PathBuf::from(vault);
                match obsidian::create_note(&vault_path, &processed, None, dry_run) {
                    Ok(path) => {
                        if dry_run {
                            log::info!("  📝 [Simulado] Note would be created: {:?}", path);
                        } else {
                            log::info!("  📝 Note created: {:?}", path);
                        }
                        note_path = Some(path.to_string_lossy().to_string());
                    }
                    Err(e) => {
                        log::error!("  ❌ Failed to create note: {e}");
                    }
                }
            }

            // Mark as processed
            if dry_run {
                log::info!("  💾 [Simulado] Video would be marked as processed in local state");
            } else {
                state.mark_processed(video.id.clone());
                save_state(&state_path, &state)?;
            }

            // Print report
            println!("══════════════════════════════════════════════════════");
            if dry_run {
                println!("  ✅ PROCESAMIENTO EXITOSO (MODO SIMULACIÓN)");
            } else {
                println!("  ✅ PROCESAMIENTO EXITOSO");
            }
            println!("══════════════════════════════════════════════════════");
            if let Some(ref path) = note_path {
                if dry_run {
                    println!("  📝  [Simulado] Nota: {}", path);
                } else {
                    println!("  📝  Nota: {}", path);
                }
            } else {
                println!("  📝  Categoría: {}", processed.target_folder);
            }
            println!("  🔗  https://youtube.com/watch?v={}", video.id);
            println!("  🏷️  {}", processed.classification.tags.join(", "));
            println!("  💡  {}", processed.classification.suggested_action);
            println!();
            println!("  Resumen:");
            for line in processed.summary.lines() {
                println!("    {}", line);
            }
            println!();
            println!("══════════════════════════════════════════════════════");
        }
        Err(e) => {
            log::error!("  ❌ Failed to process '{}': {e}", video.title);
            anyhow::bail!("Processing failed: {e}");
        }
    }

    Ok(())
}

async fn cmd_sync_patterns(force: bool) -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        anyhow::bail!("HOME environment variable is not set");
    }
    
    let target_dir = std::path::PathBuf::from(&home)
        .join(".config")
        .join("fabric")
        .join("patterns");

    if target_dir.exists() && !force {
        println!("⚠️  El directorio de patrones ya existe en {:?}", target_dir);
        println!("   Usa `yt2action sync-patterns --force` para forzar la actualización.");
        return Ok(());
    }

    println!("📥 Sincronizando patrones de Fabric desde el repositorio oficial...");
    
    // Create a temporary directory in the workspace
    let tmp_dir = std::path::PathBuf::from("/tmp/yt2action_fabric_sync");
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir)?;

    println!("   Clonando https://github.com/danielmiessler/fabric.git (shallow clone)...");
    
    // Run git clone --depth 1
    let status = std::process::Command::new("git")
        .args(&["clone", "--depth", "1", "https://github.com/danielmiessler/fabric.git", "."])
        .current_dir(&tmp_dir)
        .status()?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("Error: Falló la clonación del repositorio de Fabric.");
    }

    let source_patterns = tmp_dir.join("patterns");
    if !source_patterns.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("Error: No se encontró la carpeta 'patterns' en el repositorio clonado.");
    }

    println!("   Copiando patrones a {:?}", target_dir);
    
    // Create target dir if it doesn't exist
    std::fs::create_dir_all(&target_dir)?;

    // Copy patterns recursively
    copy_dir_all(&source_patterns, &target_dir)?;

    // Clean up
    let _ = std::fs::remove_dir_all(&tmp_dir);

    println!("✅ ¡Sincronización completada con éxito!");
    println!("   Los patrones de Fabric están listos en {:?}", target_dir);

    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn extract_video_id(input: &str) -> String {
    let input = input.trim();
    if input.contains("youtube.com/watch") {
        if let Some(pos) = input.find("v=") {
            let start = pos + 2;
            let end = input[start..].find('&').map(|idx| start + idx).unwrap_or(input.len());
            return input[start..end].to_string();
        }
    } else if input.contains("youtu.be/") {
        if let Some(pos) = input.rfind('/') {
            let start = pos + 1;
            let end = input[start..].find('?').map(|idx| start + idx).unwrap_or(input.len());
            return input[start..end].to_string();
        }
    }
    input.to_string()
}

fn get_video_metadata_ytdlp(yt_dlp_path: &str, video_id: &str) -> anyhow::Result<crate::types::Video> {
    let url = format!("https://www.youtube.com/watch?v={}", video_id);
    let output = std::process::Command::new(yt_dlp_path)
        .args(&["--dump-json", "--skip-download", &url])
        .output()
        .context("Failed to execute yt-dlp to fetch video metadata")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("yt-dlp failed to get metadata: {}", stderr.trim());
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&json_str)
        .context("Failed to parse yt-dlp metadata JSON")?;

    let id = json["id"].as_str().unwrap_or(video_id).to_string();
    let title = json["title"].as_str().unwrap_or("").to_string();
    let channel = json["channel"].as_str().or_else(|| json["uploader"].as_str()).unwrap_or("").to_string();
    let description = json["description"].as_str().unwrap_or("").to_string();
    
    // Parse duration
    let duration_seconds = json["duration"].as_u64().or_else(|| json["duration"].as_f64().map(|f| f as u64));

    // Metrics
    let view_count = json["view_count"].as_u64();
    let like_count = json["like_count"].as_u64();
    let comment_count = json["comment_count"].as_u64();

    // Published date (yt-dlp outputs "YYYYMMDD", let's format it as "YYYY-MM-DD")
    let raw_date = json["upload_date"].as_str().unwrap_or("");
    let published_at = if raw_date.len() == 8 {
        format!("{}-{}-{}", &raw_date[0..4], &raw_date[4..6], &raw_date[6..8])
    } else {
        raw_date.to_string()
    };

    Ok(crate::types::Video {
        id,
        playlist_item_id: String::new(),
        title,
        channel,
        description,
        published_at,
        duration_seconds,
        view_count,
        like_count,
        dislike_count: None,
        comment_count,
    })
}

fn log_fabric_output_temp(
    video: &crate::types::Video,
    pattern_name: &str,
    output: &str,
) -> anyhow::Result<()> {
    let target_dir = std::path::Path::new("target");
    if !target_dir.exists() {
        std::fs::create_dir_all(target_dir)?;
    }
    let log_path = target_dir.join("fabric_outputs_temp.log");
    
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
        
    let now = chrono::Local::now().to_rfc3339();
    
    writeln!(file, "================================================================================")?;
    writeln!(file, "Date/Time:    {}", now)?;
    writeln!(file, "Video ID:     {}", video.id)?;
    writeln!(file, "Video Title:  {}", video.title)?;
    writeln!(file, "Pattern:      {}", pattern_name)?;
    writeln!(file, "================================================================================")?;
    writeln!(file, "{}\n", output)?;
    
    Ok(())
}
