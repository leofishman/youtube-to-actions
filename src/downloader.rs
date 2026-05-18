use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Result of downloading a video
pub struct DownloadResult {
    /// Directory where the video is stored (e.g. ~/Videos/yt2action/<video_id>/)
    pub video_dir: PathBuf,
    /// Path to the downloaded video file
    pub video_path: Option<PathBuf>,
    /// Path to the subtitle file (if available)
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
) -> Result<DownloadResult> {
    let video_dir = videos_dir.join(video_id);
    std::fs::create_dir_all(&video_dir)
        .with_context(|| format!("Failed to create video dir: {:?}", video_dir))?;

    let output_template = video_dir.join("%(id)s.%(ext)s");
    let output_str = output_template.to_string_lossy().to_string();

    // Build subtitle language argument
    let langs = subtitle_langs.join(",");
    let sub_langs_arg = format!("--sub-langs={}", langs);

    // JavaScript runtime flags for YouTube (required for format extraction)
    let js_flags = [
        "--js-runtimes", "node",
        "--remote-components", "ejs:github",
    ];

    // Step 1: Download subtitles only (fast path)
    let sub_output = std::process::Command::new(yt_dlp_path)
        .args([
            "--write-subs",
            "--write-auto-subs",
            &sub_langs_arg,
            "--skip-download",
            "--sleep-requests", "3",
            "--no-warnings",
            "--print", "after_move:filepath",
        ])
        .args(&js_flags)
        .arg("-o")
        .arg(&output_str)
        .arg(format!("https://www.youtube.com/watch?v={}", video_id))
        .output()
        .context("Failed to execute yt-dlp for subtitles")?;

    let _sub_stdout = String::from_utf8_lossy(&sub_output.stdout);
    let sub_stderr = String::from_utf8_lossy(&sub_output.stderr);

    if !sub_output.status.success() {
        log::warn!(
            "yt-dlp subtitle download exited with {}: {}",
            sub_output.status,
            sub_stderr.trim()
        );
    }

    // Find the best subtitle file (prefer non-auto-generated)
    let subtitle_path = find_best_subtitle(&video_dir, subtitle_langs);

    // Step 2: Download video (lowest quality, we only need audio)
    let video_path = download_actual_video(yt_dlp_path, video_id, &video_dir, quality, subtitle_langs);

    // Step 3: Parse transcript from subtitles
    let transcript = subtitle_path.as_ref().and_then(|srt_path| {
        parse_vtt_or_srt(srt_path).ok()
    });

    log::info!("  📥 Video stored in: {:?}", video_dir);

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

/// Find the best subtitle file in the video directory
fn find_best_subtitle(dir: &Path, preferred_langs: &[&str]) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;

    let mut candidates: Vec<(PathBuf, bool, usize)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            if ext_str == "vtt" || ext_str == "srt" {
                let fname = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_auto = fname.contains(".en") || preferred_langs.iter().any(|l| fname.contains(l));
                let score = preferred_langs.iter().position(|l| fname.contains(l)).unwrap_or(999);
                candidates.push((path, is_auto, score));
            }
        }
    }

    // Sort: first by language priority, then prefer non-auto
    candidates.sort_by(|a, b| a.2.cmp(&b.2).then(a.1.cmp(&b.1)));

    candidates.first().map(|c| c.0.clone())
}

/// Parse a VTT or SRT subtitle file into plain text
pub fn parse_vtt_or_srt(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read subtitle file: {:?}", path))?;

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "vtt" => parse_webvtt(&content),
        "srt" => parse_srt(&content),
        _ => anyhow::bail!("Unsupported subtitle format: {}", ext),
    }
}

/// Parse WebVTT format into plain text
fn parse_webvtt(content: &str) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty()
            || trimmed.starts_with("WEBVTT")
            || trimmed.starts_with("Kind:")
            || trimmed.starts_with("Language:")
            || trimmed.starts_with("-->")
            || trimmed.starts_with("[")
            || trimmed.starts_with("♪")
            || trimmed.starts_with("NOTE")
        {
            continue;
        }

        if trimmed.contains("-->") {
            continue;
        }

        if trimmed.len() <= 12 && trimmed.chars().any(|c| c == ':') && trimmed.chars().any(|c| c == '.') {
            continue;
        }

        let cleaned = trimmed
            .replace("<c>", "")
            .replace("</c>", "")
            .replace("<v>", "")
            .replace("</v>", "")
            .replace("&nbsp;", " ")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">");

        lines.push(cleaned);
    }

    let text = lines.join(" ");
    let collapsed: String = text
        .chars()
        .fold(String::with_capacity(text.len()), |mut acc, c| {
            if c == ' ' && acc.ends_with(' ') {
                // skip duplicate space
            } else {
                acc.push(c);
            }
            acc
        });

    Ok(collapsed.trim().to_string())
}

/// Parse SRT format into plain text
fn parse_srt(content: &str) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty()
            || trimmed.chars().all(|c| c.is_ascii_digit())
            || trimmed.contains("-->")
            || trimmed.contains("♪")
        {
            continue;
        }

        lines.push(trimmed.to_string());
    }

    let text = lines.join(" ");
    let collapsed: String = text
        .chars()
        .fold(String::with_capacity(text.len()), |mut acc, c| {
            if c == ' ' && acc.ends_with(' ') {
                // skip
            } else {
                acc.push(c);
            }
            acc
        });

    Ok(collapsed.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_webvtt() {
        let vtt = r#"WEBVTT
Kind: captions
Language: en

00:00:01.360 --> 00:00:03.040
♪♪♪

00:00:18.640 --> 00:00:21.880
We're no strangers to love

00:00:22.640 --> 00:00:26.960
You know the rules and so do I
"#;

        let result = parse_webvtt(vtt).unwrap();
        assert!(result.contains("We're no strangers to love"));
        assert!(result.contains("You know the rules"));
        assert!(!result.contains("♪"));
        assert!(!result.contains("WEBVTT"));
    }

    #[test]
    fn test_parse_srt() {
        let srt = "1\n00:00:01,360 --> 00:00:03,040\nHello world\n\n2\n00:00:04,000 --> 00:00:06,000\nThis is a test\n";

        let result = parse_srt(srt).unwrap();
        assert!(result.contains("Hello world"));
        assert!(result.contains("This is a test"));
    }
}
