mod config;
mod downloader;
mod obsidian;
mod processor;
mod security;
mod sp_api;
mod types;
mod youtube;

use anyhow::Context;
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
use types::ProcessResult;
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

        /// Only process N videos (default: 5)
        #[arg(short, long, default_value = "5")]
        limit: usize,

        /// Reprocess videos even if already processed
        #[arg(short, long)]
        force: bool,

        /// Apply Fabric patterns for rich content analysis (comma-separated or multiple flags, e.g. "summarize,extract_wisdom")
        #[arg(short, long, visible_alias = "pattern", value_delimiter = ',', num_args = 1..)]
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
        #[arg(short, long, visible_alias = "pattern", value_delimiter = ',', num_args = 1..)]
        patterns: Vec<String>,

        /// Actually download the video file locally (default: false, only fetches transcript)
        #[arg(long)]
        download_video: bool,

        /// Dry run simulation: fetch and process with LLM, but do not write notes, tasks, or alter state
        #[arg(long)]
        dry_run: bool,

        /// Imprime el resumen por stdout (logs a stderr) y no escribe nota, estado, tarea ni archivos
        #[arg(long, visible_alias = "no-write")]
        stdout: bool,

        /// Reprocess even if the video is already in the processed history
        #[arg(long)]
        force: bool,
    },
    /// Synchronize Fabric patterns from the official repository to ~/.config/fabric/patterns/
    SyncPatterns {
        /// Force update and overwrite existing patterns
        #[arg(short, long)]
        force: bool,
    },
    /// Manage local processed video state/history
    State {
        #[command(subcommand)]
        subcommand: StateSubcommands,
    },
}

#[derive(Subcommand)]
enum StateSubcommands {
    /// Show all processed video IDs in local state
    Show,
    /// Clear the entire processed history
    Reset {
        /// Force reset without confirmation prompt
        #[arg(short, long)]
        force: bool,
    },
    /// Unmark a specific video ID (remove it from processed history)
    Unmark {
        /// Video ID or YouTube URL to remove
        video: String,
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
        Commands::Process {
            video,
            config,
            patterns,
            download_video,
            dry_run,
            stdout,
            force,
        } => {
            cmd_process(
                video,
                config,
                patterns,
                download_video,
                dry_run,
                stdout,
                force,
            )
            .await
        }
        Commands::SyncPatterns { force } => cmd_sync_patterns(force).await,
        Commands::State { subcommand } => cmd_state(subcommand).await,
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
    let mut state = load_state(&state_path)?;

    // Collect playlists to process with their SP settings and overrides
    let playlists_to_process: Vec<_> = {
        let mut playlists = Vec::new();

        // Add legacy default playlist if no other playlists are configured
        if cfg.youtube.playlists.is_empty() {
            playlists.push((
                cfg.youtube.playlist_id.clone(),
                vec![],
                None,                             // sp_enabled - use global
                cfg.output.sp_project_id.clone(), // sp_project_id - use global
                None,                             // obsidian_folder - use default
                None,                             // obsidian_vault - use default
                None,                             // max_transcript_chars - use global
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
    for (
        playlist_id,
        playlist_patterns,
        playlist_sp_enabled,
        playlist_project_id,
        playlist_obsidian_folder,
        playlist_obsidian_vault,
        playlist_max_transcript_chars,
    ) in playlists_to_process
    {
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
            cfg.processing.llm_api_key.as_deref().unwrap_or(""),
            valid_folders,
            playlist_max_transcript_chars.or(cfg.processing.max_transcript_chars),
        );

        // Resolve yt-dlp path and storage config
        let yt_dlp_path = cfg.yt_dlp_path();
        let audio_fallback = cfg.audio_fallback()?;
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
            let transcript_result = match downloader::download_video(
                &yt_dlp_path,
                &video.id,
                &videos_dir,
                &quality,
                &subtitle_langs,
                download_video_flag,
                &audio_fallback,
            )
            .await
            {
                Ok(t) => t,
                // Un video que falla no corta los demás (auditoría #6)
                Err(e) => {
                    log::error!(
                        "  ❌ Failed to fetch transcript for '{}': {e:#}",
                        video.title
                    );
                    results.push(error_result(video, &e));
                    continue;
                }
            };

            // Step 2: AI processing
            match proc
                .process(video, transcript_result.transcript.as_deref())
                .await
            {
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

                    let (pattern_execution_times, pattern_errors) = run_patterns(
                        &proc,
                        &mut processed,
                        &patterns_to_run,
                        &fabric_patterns_dir(),
                        true,
                    )
                    .await;
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
                        pattern_errors,
                    };

                    // Nota en Obsidian (salida principal), marca en el estado y mover de
                    // playlist. Si la nota falla, el video queda como error: sin tarea, sin
                    // mover y sin marcar.
                    let active_vault = playlist_obsidian_vault
                        .as_ref()
                        .or(cfg.output.obsidian_vault.as_ref())
                        .map(PathBuf::from);
                    // Mover a la playlist de procesados, si hay una configurada
                    let move_video = async || -> anyhow::Result<bool> {
                        let Some(processed_pl) = cfg.youtube.processed_playlist_id.as_ref() else {
                            return Ok(false);
                        };
                        if dry_run {
                            log::info!(
                                "  ✅ [Simulado] Video would be moved to processed playlist: {processed_pl}"
                            );
                            return Ok(true);
                        }
                        yt.add_to_playlist(processed_pl, &video.id)
                            .await
                            .context("Could not add to processed playlist")?;
                        yt.remove_from_playlist(&video.playlist_item_id)
                            .await
                            .context(
                                "Added to processed playlist but could not remove from source",
                            )?;
                        log::info!("  ✅ Moved to processed playlist: {processed_pl}");
                        Ok(true)
                    };
                    match finish_video(
                        active_vault.as_deref(),
                        &processed,
                        playlist_obsidian_folder.as_deref(),
                        dry_run,
                        &mut state,
                        &state_path,
                        move_video,
                    )
                    .await
                    {
                        Ok((path, moved)) => {
                            result.note_path = path.map(|p| p.to_string_lossy().to_string());
                            result.moved_to_processed = moved;
                        }
                        Err(e) => {
                            log::error!(
                                "  ❌ Nota o estado fallaron, el video no queda procesado: {e:#}"
                            );
                            result.error = Some(format!("{e:#}"));
                            results.push(result);
                            continue;
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
                                log::debug!(
                                    "Resolved SP project '{}' to ID: {}",
                                    project_id_raw,
                                    id
                                );
                                id
                            }
                            _ => {
                                log::debug!(
                                    "Could not resolve SP project '{}', using raw value",
                                    project_id_raw
                                );
                                project_id_raw
                            }
                        };

                        let title =
                            format!("[📺] {}", security::strip_html(&processed.video.title));
                        let notes = format!(
                            "{}\n\n## Puntos clave\n{}\n\n🔗 https://youtube.com/watch?v={}",
                            security::strip_html(&processed.summary),
                            processed
                                .key_points
                                .iter()
                                .map(|p| format!("- {}", security::strip_html(p)))
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

                    results.push(result);
                }
                Err(e) => {
                    log::error!("  ❌ Failed to process '{}': {e}", video.title);
                    results.push(error_result(video, &e));
                }
            }
        }

        total_processed += to_process.len();
    }

    // El estado ya se guardó después de cada video (record_note)
    if dry_run {
        log::info!(
            "💾 [Simulado] State changes would be saved to {:?}",
            state_path
        );
    }

    // --- FINAL REPORT ---
    print_report(&results, &cfg, start_time.elapsed(), dry_run);

    Ok(())
}

fn error_result(video: &types::Video, e: &anyhow::Error) -> ProcessResult {
    ProcessResult {
        video_title: video.title.clone(),
        video_url: format!("https://youtube.com/watch?v={}", video.id),
        target_folder: String::new(),
        note_path: None,
        sp_task_id: None,
        moved_to_processed: false,
        suggested_action: types::SuggestedAction::SaveForLater,
        tags: vec![],
        error: Some(format!("{e:#}")),
        pattern_execution_times: vec![],
        pattern_errors: vec![],
    }
}

/// Print a beautiful summary report after processing
fn print_report(
    results: &[ProcessResult],
    cfg: &Config,
    total_duration: std::time::Duration,
    dry_run: bool,
) {
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
        for err in &r.pattern_errors {
            println!("     ⚠️  Patrón fallido: {err}");
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
        types::SuggestedAction::WatchFull => Some("👀  Sugerencia: Ver completo (vale la pena)"),
        types::SuggestedAction::ReadTranscript => Some("📖  Sugerencia: Solo leer resumen"),
        types::SuggestedAction::SaveForLater => Some("💾  Sugerencia: Guardar para después"),
        types::SuggestedAction::Archive => Some("📦  Sugerencia: Archivar (referencia)"),
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
    // /health no pide token: responde OK aunque no podamos crear ni una tarea.
    // Desde SP v19 el resto de la API es Bearer, así que el health check tiene que
    // tocar una ruta autenticada o no detecta el único modo de falla que importa.
    match sp.list_projects().await {
        Ok(projects) => println!("   ✅ Autenticado ({} proyectos visibles).", projects.len()),
        Err(e) => println!("   ❌ Sin autenticación: {e}"),
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

# Videos SIN subtítulos: se baja solo el audio con yt-dlp (una vez, con límites) y se
# transcribe con este comando. Plantilla: {audio} = archivo de audio; {out} (opcional) = archivo
# de salida; sin {out} la transcripción se lee de stdout. Sin esto, esos videos dan error.
# Servicio faster-whisper compatible con OpenAI. Si tu servicio de ASR pide credencial, no la
# pongas en este archivo: leela de un archivo o variable fuera del repo. Alternativa: whisper.cpp, ver README.
# asr_command = 'curl -sS -m 3600 -F file=@{audio} -F model=whisper-1 -F language=es -F response_format=text http://192.168.1.150:8091/v1/audio/transcriptions'
# Tope diario de ejecuciones de yt-dlp (audio de respaldo y --download-video juntos)
# max_downloads_per_day = 5
# Cookies del navegador para yt-dlp (solo en ese respaldo; úsalo si YouTube pide "Sign in")
# yt_dlp_cookies_from_browser = "firefox"

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

fn load_state(path: &std::path::Path) -> anyhow::Result<types::ProcessState> {
    if !path.exists() {
        return Ok(types::ProcessState::new());
    }
    // Corrupto o ilegible: error, nunca un estado vacío que después se guarde encima
    // y haga reprocesar todo (auditoría #8).
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("No se pudo leer el estado {path:?}"))?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "El estado {path:?} está corrupto; no se toca. Revisalo a mano, o borralo con \
             `yt2action state reset` (se reprocesan todos los videos)"
        )
    })
}

fn save_state(path: &std::path::Path, state: &types::ProcessState) -> anyhow::Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    // Atomic write: write to temp file first, then rename.
    // rename() is atomic on Linux (ext4/btrfs), so if the process crashes
    // mid-write, the original state.json remains intact.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("Failed to write temp state file: {:?}", tmp_path))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to rename temp state to: {:?}", path))?;
    Ok(())
}

/// Process a single video by ID, URL, or position in playlist
async fn cmd_process(
    video_arg: String,
    config_path: Option<PathBuf>,
    patterns: Vec<String>,
    download_video_flag: bool,
    dry_run: bool,
    to_stdout: bool,
    force: bool,
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

        log::info!(
            "Fetching playlist {} to resolve position...",
            cfg.youtube.playlist_id
        );
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
        log::info!(
            "Fetching public video details via yt-dlp for ID: {}...",
            resolved_id
        );
        get_video_metadata_ytdlp(&cfg.yt_dlp_path(), &resolved_id)?
    };

    if !to_stdout {
        println!("══════════════════════════════════════════════════════");
        println!("  📺 PROCESANDO VIDEO INDIVIDUAL");
        println!("══════════════════════════════════════════════════════");
        println!("  Título: {}", video.title);
        println!("  ID: {}", video.id);
        println!("  Canal: {}", video.channel);
        println!();
    }

    // Load state
    let state_path = get_state_path()?;
    // --stdout no toca el estado: no lo lee (un state.json corrupto no lo bloquea)
    let mut state = if to_stdout {
        types::ProcessState::new()
    } else {
        load_state(&state_path)?
    };

    // Igual que `run`: lo ya procesado se saltea salvo con --force (la simulación y --stdout no tocan el estado)
    if !force && !to_stdout && !dry_run && state.is_processed(&video.id) {
        println!(
            "  ⏭️  Ya procesado: {}. Usá --force para reprocesarlo.",
            video.id
        );
        return Ok(());
    }

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
        cfg.processing.llm_api_key.as_deref().unwrap_or(""),
        valid_folders,
        cfg.processing.max_transcript_chars,
    );

    // Resolve yt-dlp path and storage config
    let yt_dlp_path = cfg.yt_dlp_path();
    let audio_fallback = cfg.audio_fallback()?;
    // --stdout: caché de transcripción en un directorio temporal propio, se borra al final
    let videos_dir = if to_stdout {
        std::env::temp_dir().join(format!("yt2action-stdout-{}", std::process::id()))
    } else {
        cfg.videos_dir()?
    };
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
        &audio_fallback,
    )
    .await;
    if to_stdout {
        let _ = std::fs::remove_dir_all(&videos_dir);
    }
    let transcript_result = transcript_result?;

    // AI processing
    match proc
        .process(&video, transcript_result.transcript.as_deref())
        .await
    {
        Ok(mut processed) => {
            // Set local video path
            processed.local_video_path = transcript_result
                .video_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string());

            // If Fabric patterns are specified, run the second phase
            let (_, pattern_errors) = run_patterns(
                &proc,
                &mut processed,
                &patterns,
                &fabric_patterns_dir(),
                !to_stdout,
            )
            .await;

            if to_stdout {
                // Salida para otros programas: un patrón fallido es error, no salida parcial (auditoría #4)
                anyhow::ensure!(
                    pattern_errors.is_empty(),
                    "Fallaron patrones de Fabric: {}",
                    pattern_errors.join("; ")
                );

                println!("{}", processed.summary);
                if !processed.key_points.is_empty() {
                    println!();
                    for kp in &processed.key_points {
                        println!("- {kp}");
                    }
                }
                if let Some(ref f) = processed.fabric_output {
                    println!("\n{f}");
                }
                return Ok(());
            }

            // Nota + estado. Si la nota falla: error, sin marcar ni imprimir éxito.
            let vault = cfg.output.obsidian_vault.as_ref().map(PathBuf::from);
            let (note_path, _) = finish_video(
                vault.as_deref(),
                &processed,
                None,
                dry_run,
                &mut state,
                &state_path,
                async || Ok(false),
            )
            .await
            .context("No se pudo crear la nota; el video no se marcó como procesado")?;
            let note_path = note_path.map(|p| p.to_string_lossy().to_string());

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
            for err in &pattern_errors {
                println!("  ⚠️  Patrón fallido (no está en la nota): {err}");
            }
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

fn fabric_patterns_dir() -> String {
    format!(
        "{}/.config/fabric/patterns",
        std::env::var("HOME").unwrap_or_default()
    )
}

/// Corre los patrones de Fabric y agrega su salida a `processed.fabric_output`.
/// Devuelve los tiempos por patrón y los que fallaron o volvieron vacíos (auditoría #4),
/// para que el que llama los muestre: no son un éxito silencioso.
async fn run_patterns(
    proc: &processor::Processor,
    processed: &mut types::ProcessedVideo,
    patterns: &[String],
    patterns_dir: &str,
    log_to_file: bool,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut times = Vec::new();
    let mut errors = Vec::new();
    for pattern_name in patterns {
        log::info!("  🧠 Ejecutando patrón de Fabric: {}...", pattern_name);
        let start = std::time::Instant::now();
        let res = proc
            .process_with_pattern(
                &processed.video,
                processed.transcript.as_deref(),
                pattern_name,
                patterns_dir,
            )
            .await;
        times.push((
            pattern_name.clone(),
            format!("{:.1}s", start.elapsed().as_secs_f64()),
        ));

        match res {
            Ok(fabric_output) => {
                match processed.fabric_output.as_mut() {
                    None => processed.fabric_output = Some(fabric_output.clone()),
                    Some(current) => current.push_str(&format!(
                        "\n\n---\n\n# Patrón: {}\n\n{}",
                        pattern_name, fabric_output
                    )),
                }
                log::info!("  📜 Fabric pattern output stored");

                // Log temporal de salidas de Fabric
                if log_to_file {
                    if let Err(e) =
                        log_fabric_output_temp(&processed.video, pattern_name, &fabric_output)
                    {
                        log::warn!("  ⚠️ No se pudo guardar el log temporal de Fabric: {e}");
                    }
                }
            }
            Err(e) => {
                log::error!("  ❌ Falló el patrón de Fabric {pattern_name}: {e:#}");
                errors.push(format!("{pattern_name}: {e:#}"));
            }
        }
    }
    (times, errors)
}

/// Cierre de un video ya analizado, compartido por `run` y `process`: nota en
/// Obsidian, marca en el estado (guardada) y mover de playlist. Si la nota falla
/// devuelve error sin marcar (auditoría #2). Devuelve la nota y si se movió.
async fn finish_video(
    vault: Option<&std::path::Path>,
    processed: &types::ProcessedVideo,
    folder: Option<&str>,
    dry_run: bool,
    state: &mut types::ProcessState,
    state_path: &std::path::Path,
    move_video: impl AsyncFnOnce() -> anyhow::Result<bool>,
) -> anyhow::Result<(Option<PathBuf>, bool)> {
    let note = match vault {
        Some(vault) => Some(obsidian::create_note(vault, processed, folder, dry_run)?),
        None => None,
    };
    // Se marca y guarda siempre, por video (auditoría #6) y ANTES de mover: si el
    // add/remove de playlist falla, el video no queda para reprocesar (auditoría #9).
    if dry_run {
        log::info!("  💾 [Simulado] Video would be marked as processed in local state");
    } else {
        state.mark_processed(processed.video.id.clone());
        save_state(state_path, state)?;
    }
    let moved = match move_video().await {
        Ok(moved) => moved,
        Err(e) => {
            log::warn!("  ⚠️ {e:#}");
            false
        }
    };
    Ok((note, moved))
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
        println!(
            "⚠️  El directorio de patrones ya existe en {:?}",
            target_dir
        );
        println!("   Usa `yt2action sync-patterns --force` para forzar la actualización.");
        return Ok(());
    }

    println!("📥 Sincronizando patrones de Fabric desde el repositorio oficial...");

    let tmp_dir = unique_tmp_dir("yt2action-fabric")?;
    let res = fetch_fabric_patterns(&tmp_dir, &target_dir);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    res?;

    println!("✅ ¡Sincronización completada con éxito!");
    println!("   Los patrones de Fabric están listos en {:?}", target_dir);

    Ok(())
}

/// Clona solo las carpetas de patrones de Fabric en `tmp_dir` (sparse checkout: ~4 MB
/// contra ~47 MB del clon superficial completo) y las copia a `target_dir`.
fn fetch_fabric_patterns(
    tmp_dir: &std::path::Path,
    target_dir: &std::path::Path,
) -> anyhow::Result<()> {
    println!(
        "   Clonando las carpetas de patrones de https://github.com/danielmiessler/fabric.git ..."
    );
    let git = |args: &[&str]| -> anyhow::Result<()> {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp_dir)
            .status()
            .context("No se pudo ejecutar git")?;
        anyhow::ensure!(status.success(), "Error: falló `git {}`", args.join(" "));
        Ok(())
    };
    git(&[
        "clone",
        "-q",
        "--depth",
        "1",
        "--filter=blob:none",
        "--sparse",
        "https://github.com/danielmiessler/fabric.git",
        ".",
    ])?;
    git(&["sparse-checkout", "set", "data/patterns", "patterns"])?;

    let source_patterns = find_patterns_dir(tmp_dir).context(
        "Error: no se encontró 'data/patterns' ni 'patterns' en el repositorio de Fabric.",
    )?;
    println!(
        "   Copiando patrones de {:?} a {:?}",
        source_patterns, target_dir
    );
    copy_dir_all(&source_patterns, target_dir)?;
    Ok(())
}

/// Carpeta de patrones en un clon de Fabric: `data/patterns/` (actual) o `patterns/` (versiones viejas).
fn find_patterns_dir(repo: &std::path::Path) -> Option<PathBuf> {
    ["data/patterns", "patterns"]
        .into_iter()
        .map(|p| repo.join(p))
        .find(|p| p.is_dir())
}

/// Directorio temporal nuevo y propio. `create_dir` falla si ya existe: nunca reusa ni
/// borra uno ajeno (antes era un `/tmp/yt2action_fabric_sync` fijo).
fn unique_tmp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&d)?;
    Ok(d)
}

async fn cmd_state(subcommand: StateSubcommands) -> anyhow::Result<()> {
    let state_path = get_state_path()?;
    // reset no lee el estado: es la salida explícita para un state.json corrupto
    let load = || load_state(&state_path);

    match subcommand {
        StateSubcommands::Show => {
            let state = load()?;
            if state.processed_ids.is_empty() {
                println!("No hay videos registrados en el historial de procesados.");
            } else {
                println!(
                    "Historial de videos procesados ({} total):",
                    state.processed_ids.len()
                );
                for id in &state.processed_ids {
                    println!("  - https://youtube.com/watch?v={}", id);
                }
            }
        }
        StateSubcommands::Reset { force } => {
            if !force {
                print!("¿Estás seguro de que quieres borrar todo el historial? (s/N): ");
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if input.trim().to_lowercase() != "s" {
                    println!("Operación cancelada.");
                    return Ok(());
                }
            }
            save_state(&state_path, &types::ProcessState::new())?;
            println!("✅ Historial de videos procesados borrado con éxito.");
        }
        StateSubcommands::Unmark { video } => {
            let mut state = load()?;
            let id = extract_video_id(&video);
            if state.is_processed(&id) {
                state.processed_ids.retain(|x| x != &id);
                save_state(&state_path, &state)?;
                println!("✅ Video {} eliminado del historial de procesados.", id);
            } else {
                println!(
                    "⚠️ El video {} no se encuentra en el historial de procesados.",
                    id
                );
            }
        }
    }

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
            let end = input[start..]
                .find('&')
                .map(|idx| start + idx)
                .unwrap_or(input.len());
            return input[start..end].to_string();
        }
    } else if input.contains("youtu.be/") {
        if let Some(pos) = input.rfind('/') {
            let start = pos + 1;
            let end = input[start..]
                .find('?')
                .map(|idx| start + idx)
                .unwrap_or(input.len());
            return input[start..end].to_string();
        }
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::test_util::{processed_video, tmp_dir};

    // Auditoría #2: si la nota no se puede escribir, el video no queda procesado.
    #[tokio::test]
    async fn nota_fallida_no_marca_procesado() {
        let dir = tmp_dir("nota-fallida");
        let vault = dir.join("vault");
        std::fs::write(&vault, "un archivo, no un directorio").unwrap();
        let mut state = types::ProcessState::new();
        let r = finish_video(
            Some(&vault),
            &processed_video("abc", "T"),
            None,
            false,
            &mut state,
            &dir.join("state.json"),
            async || Ok(false),
        )
        .await;
        assert!(r.is_err(), "la nota falló: no es éxito");
        assert!(!state.is_processed("abc"));
    }

    // Auditoría #8: un state.json corrupto es un error claro y no se pisa.
    #[test]
    fn estado_corrupto_es_error_y_no_se_pisa() {
        let path = tmp_dir("estado-corrupto").join("state.json");
        std::fs::write(&path, "{\"processed_ids\": [\"a\", ").unwrap();
        let e = load_state(&path).expect_err("corrupto no es un estado vacío");
        assert!(format!("{e:#}").contains("state.json"), "{e:#}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"processed_ids\": [\"a\", "
        );
    }

    // Auditoría #9: con playlist de procesados, el estado se marca y guarda ANTES de
    // mover, y un add/remove fallido no deja el video para reprocesar.
    #[tokio::test]
    async fn con_playlist_de_procesados_marca_antes_de_mover() {
        let dir = tmp_dir("mover");
        let state_path = dir.join("state.json");
        let mut state = types::ProcessState::new();
        let sp = state_path.clone();
        let r = finish_video(
            Some(&dir.join("vault")),
            &processed_video("abc", "T"),
            None,
            false,
            &mut state,
            &state_path,
            async || {
                let on_disk = std::fs::read_to_string(&sp).unwrap_or_default();
                anyhow::ensure!(on_disk.contains("abc"), "se movió sin marcar antes");
                anyhow::bail!("add_to_playlist falló")
            },
        )
        .await;
        let (_, moved) = r.expect("un fallo al mover no es un fallo del video");
        assert!(!moved);
        assert!(
            state.is_processed("abc"),
            "sin marca, el próximo run lo reprocesa"
        );
        assert!(
            std::fs::read_to_string(&state_path)
                .unwrap_or_default()
                .contains("abc")
        );
    }

    // Auditoría #4: un patrón vacío o que falla queda en la lista de fallidos, no en la nota.
    #[tokio::test]
    async fn patron_fallido_se_reporta() {
        let (url, _) = processor::tests::llm_server(vec![("", "stop")]);
        let proc = processor::Processor::new(&url, "m", "", vec!["Learn".into()], None);
        let mut processed = processed_video("abc", "T");
        let dir = processor::tests::patterns_dir("pat-fallido");
        let patterns = vec!["summarize".to_string(), "no_existe".to_string()];
        let (times, errors) = run_patterns(&proc, &mut processed, &patterns, &dir, false).await;
        assert_eq!(times.len(), 2);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].starts_with("summarize:") && errors[1].starts_with("no_existe:"));
        assert!(processed.fabric_output.is_none());
    }

    // Auditoría #6: el estado se guarda en disco después de cada video, no al final del run.
    #[tokio::test]
    async fn estado_se_guarda_tras_cada_video() {
        let dir = tmp_dir("estado-por-video");
        let state_path = dir.join("state.json");
        let mut state = types::ProcessState::new();
        for id in ["v1", "v2"] {
            finish_video(
                Some(&dir.join("vault")),
                &processed_video(id, id),
                None,
                false,
                &mut state,
                &state_path,
                async || Ok(false),
            )
            .await
            .unwrap();
            let on_disk = std::fs::read_to_string(&state_path).unwrap_or_default();
            assert!(on_disk.contains(id), "{id} tiene que estar en disco ya");
        }
    }

    // Auditoría #12: Fabric movió los patrones a data/patterns/; patterns/ queda de fallback.
    #[test]
    fn patrones_en_data_patterns_con_fallback() {
        let repo = tmp_dir("fabric-repo");
        assert_eq!(find_patterns_dir(&repo), None);
        std::fs::create_dir_all(repo.join("patterns")).unwrap();
        assert_eq!(find_patterns_dir(&repo), Some(repo.join("patterns")));
        std::fs::create_dir_all(repo.join("data/patterns")).unwrap();
        assert_eq!(find_patterns_dir(&repo), Some(repo.join("data/patterns")));
    }

    // Baja de GitHub: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn sync_patterns_contra_github() {
        let tmp = unique_tmp_dir("yt2a-test-clone").unwrap();
        let target = tmp_dir("fabric-target");
        let r = fetch_fabric_patterns(&tmp, &target);
        let _ = std::fs::remove_dir_all(&tmp);
        r.unwrap();
        assert!(target.join("summarize/system.md").is_file());
        assert!(target.join("extract_wisdom/system.md").is_file());
    }

    // Auditoría #12: el clon va a un directorio temporal propio, no a uno fijo en /tmp.
    #[test]
    fn directorio_temporal_unico() {
        let a = unique_tmp_dir("yt2a-test-sync").unwrap();
        let b = unique_tmp_dir("yt2a-test-sync").unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}

fn get_video_metadata_ytdlp(
    yt_dlp_path: &str,
    video_id: &str,
) -> anyhow::Result<crate::types::Video> {
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
    let json: serde_json::Value =
        serde_json::from_str(&json_str).context("Failed to parse yt-dlp metadata JSON")?;

    let id = json["id"].as_str().unwrap_or(video_id).to_string();
    let title = json["title"].as_str().unwrap_or("").to_string();
    let channel = json["channel"]
        .as_str()
        .or_else(|| json["uploader"].as_str())
        .unwrap_or("")
        .to_string();
    let description = json["description"].as_str().unwrap_or("").to_string();

    // Parse duration
    let duration_seconds = json["duration"]
        .as_u64()
        .or_else(|| json["duration"].as_f64().map(|f| f as u64));

    // Metrics
    let view_count = json["view_count"].as_u64();
    let like_count = json["like_count"].as_u64();
    let comment_count = json["comment_count"].as_u64();

    // Published date (yt-dlp outputs "YYYYMMDD", let's format it as "YYYY-MM-DD")
    let raw_date = json["upload_date"].as_str().unwrap_or("");
    let published_at = if raw_date.len() == 8 {
        format!(
            "{}-{}-{}",
            &raw_date[0..4],
            &raw_date[4..6],
            &raw_date[6..8]
        )
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

    writeln!(
        file,
        "================================================================================"
    )?;
    writeln!(file, "Date/Time:    {}", now)?;
    writeln!(file, "Video ID:     {}", video.id)?;
    writeln!(file, "Video Title:  {}", video.title)?;
    writeln!(file, "Pattern:      {}", pattern_name)?;
    writeln!(
        file,
        "================================================================================"
    )?;
    writeln!(file, "{}\n", output)?;

    Ok(())
}
