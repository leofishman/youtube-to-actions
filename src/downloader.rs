use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Result of downloading a video
pub struct DownloadResult {
    /// Directory where the video is stored (e.g. ~/Videos/yt2action/<video_id>/)
    #[allow(dead_code)]
    pub video_dir: PathBuf,
    /// Path to the downloaded video file
    pub video_path: Option<PathBuf>,
    /// Path to the subtitle file (if available)
    #[allow(dead_code)]
    pub subtitle_path: Option<PathBuf>,
    /// Raw transcript text extracted from subtitles
    pub transcript: Option<String>,
}

/// Download a video and its subtitles using yt-dlp
///
/// Strategy:
/// 1. Download video in lowest quality (only need audio for transcription)
/// 2. Download subtitles in preferred languages
/// 3. Store everything in ~/Videos/yt2action/<video_id>/
/// 4. Parse subtitles to extract plain text transcript
pub async fn download_video(
    yt_dlp_path: &str,
    video_id: &str,
    videos_dir: &Path,
    quality: &str,
    subtitle_langs: &[&str],
    download_video_flag: bool,
) -> Result<DownloadResult> {
    let video_dir = videos_dir.join(video_id);
    std::fs::create_dir_all(&video_dir)
        .with_context(|| format!("Failed to create video dir: {:?}", video_dir))?;

    // Step 1: Download transcript using ytt (saves to transcript.txt)
    let transcript_path = video_dir.join("transcript.txt");
    let transcript_str = transcript_path.to_string_lossy().to_string();

    let mut ytt_cmd = std::process::Command::new("ytt");
    ytt_cmd.arg(video_id);
    
    // Add each language as a separate -l argument
    for lang in subtitle_langs {
        ytt_cmd.arg("-l").arg(lang);
    }

    let sub_output = ytt_cmd
        .args(["-f", "text"])
        .args(["-o", &transcript_str])
        .output()
        .context("Failed to execute ytt for transcript")?;

    if !sub_output.status.success() {
        let sub_stderr = String::from_utf8_lossy(&sub_output.stderr);
        log::warn!(
            "ytt transcript download exited with {}: {}",
            sub_output.status,
            sub_stderr.trim()
        );
    }

    // Step 2: Download video (if requested)
    let video_path = if download_video_flag {
        download_actual_video(yt_dlp_path, video_id, &video_dir, quality, subtitle_langs)
    } else {
        log::info!("Skipping video download as per configuration.");
        None
    };

    // Step 3: Read transcript from file
    let transcript = std::fs::read_to_string(&transcript_path).ok();
    let subtitle_path = if transcript_path.exists() {
        Some(transcript_path)
    } else {
        None
    };

    if download_video_flag {
        log::info!("  📥 Video stored in: {:?}", video_dir);
    }

    Ok(DownloadResult {
        video_dir,
        video_path,
        subtitle_path,
        transcript,
    })
}

/// Download the actual video file
fn download_actual_video(
    _yt_dlp_path: &str,
    video_id: &str,
    video_dir: &Path,
    _quality: &str,
    _subtitle_langs: &[&str],
) -> Option<PathBuf> {
    // yaydl outputs mp4 by default if we don't specify only-audio
    let output_template = video_dir.join(format!("{}.mp4", video_id));
    let output_str = output_template.to_string_lossy().to_string();

    log::info!("Downloading video using yaydl...");
    let result = std::process::Command::new("yaydl")
        .arg("-o")
        .arg(&output_str)
        .arg(format!("https://www.youtube.com/watch?v={}", video_id))
        .output();

    match result {
        Ok(output) if output.status.success() => {
            // Find the video file by scanning the directory (in case yaydl changed extension)
            let video_files: Option<Vec<PathBuf>> = std::fs::read_dir(video_dir)
                .ok()
                .map(|dir| {
                    dir.filter_map(|e| e.ok())
                        .filter(|e| {
                            if let Some(ext) = e.path().extension() {
                                let ext_str = ext.to_string_lossy().to_lowercase();
                                matches!(ext_str.as_str(), "mp4" | "webm" | "avi" | "mkv" | "mov" | "mpg" | "flv")
                            } else {
                                false
                            }
                        })
                        .map(|e| e.path().clone())
                        .collect()
                });

            video_files.and_then(|v| v.into_iter().next())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("yaydl video download returned {}: {}", output.status, stderr.trim());
            None
        }
        Err(e) => {
            log::warn!("Failed to run yaydl for video download: {}", e);
            None
        }
    }
}




