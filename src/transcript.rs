use crate::downloader;
use anyhow::Result;
use std::path::PathBuf;

/// Result of fetching a transcript
pub struct TranscriptResult {
    /// Raw transcript text extracted from subtitles
    pub text: Option<String>,
    /// Path to the subtitle file (if available)
    pub subtitle_path: Option<PathBuf>,
    /// Path to the downloaded video file
    pub video_path: Option<PathBuf>,
    /// Directory where the video is stored
    pub video_dir: PathBuf,
}

/// Fetch transcript for a YouTube video by downloading it locally via yt-dlp
///
/// This replaces the old approach of using YouTube's InnerTube API (which YouTube
/// now blocks). The new approach:
///
/// 1. Download subtitles via yt-dlp (fast, small files)
/// 2. Parse the .vtt/.srt file into plain text
/// 3. Also download the video (lowest quality) for local archival
///
/// All files are stored in ~/Videos/yt2action/<video_id>/
pub async fn get_transcript(
    yt_dlp_path: &str,
    video_id: &str,
    videos_dir: &PathBuf,
    quality: &str,
    subtitle_langs: &[&str],
) -> Result<TranscriptResult> {
    let result = downloader::download_video(
        yt_dlp_path,
        video_id,
        videos_dir,
        quality,
        subtitle_langs,
    )
    .await?;

    Ok(TranscriptResult {
        text: result.transcript,
        subtitle_path: result.subtitle_path,
        video_path: result.video_path,
        video_dir: result.video_dir,
    })
}
