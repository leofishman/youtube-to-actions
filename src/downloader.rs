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

    // Cache check: skip download if transcript already exists and is non-empty
    let transcript_cached = transcript_path.exists()
        && std::fs::metadata(&transcript_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false);

    if transcript_cached {
        log::info!("  📄 Transcript cache hit: {:?}", transcript_path);
    } else {
        fetch_transcript("ytt", video_id, subtitle_langs, &transcript_path)?;
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

/// Baja la transcripción con `ytt` (o el programa que se pase, para tests) a `dest`.
/// Si `ytt` falla es error, no "sin subtítulos". `ytt` escribe a un `.part.txt` que se
/// renombra solo si terminó bien: la caché nunca ve un archivo a medias.
fn fetch_transcript(ytt: &str, video_id: &str, langs: &[&str], dest: &Path) -> Result<()> {
    let part = dest.with_extension("part.txt");
    let mut ytt_cmd = std::process::Command::new(ytt);

    // Add each language as a separate -l argument
    for lang in langs {
        ytt_cmd.arg("-l").arg(lang);
    }

    let sub_output = ytt_cmd
        .args(["-f", "text"])
        .arg("-o")
        .arg(&part)
        // `--`: un ID que empieza con '-' no se lee como flag
        .arg("--")
        .arg(video_id)
        .output()
        .context("Failed to execute ytt for transcript")?;

    if !sub_output.status.success() {
        let _ = std::fs::remove_file(&part);
        let sub_stderr = String::from_utf8_lossy(&sub_output.stderr);
        anyhow::bail!(
            "ytt falló ({}) al bajar la transcripción de {video_id}: {}",
            sub_output.status,
            sub_stderr.trim()
        );
    }
    if part.exists() {
        std::fs::rename(&part, dest)
            .with_context(|| format!("No se pudo mover {part:?} a {dest:?}"))?;
    }
    Ok(())
}

/// Download the actual video file
fn download_actual_video(
    yt_dlp_path: &str,
    video_id: &str,
    video_dir: &Path,
    quality: &str,
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

    let try_fallback = match result {
        Ok(output) if output.status.success() => {
            log::info!("  ✅ yaydl download successful.");
            false
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!(
                "  ⚠️ yaydl video download returned {}: {}",
                output.status,
                stderr.trim()
            );
            true
        }
        Err(e) => {
            log::warn!("  ⚠️ Failed to run yaydl for video download: {}", e);
            true
        }
    };

    if try_fallback {
        log::info!("  🔄 Running fallback strategy using yt-dlp...");

        // Strategy 1: Try yt-dlp with Firefox cookies
        log::info!("  Attempting yt-dlp with Firefox cookies...");
        let res_firefox = std::process::Command::new(yt_dlp_path)
            .arg("-f")
            .arg(quality)
            .arg("-o")
            .arg(&output_str)
            .arg("--cookies-from-browser")
            .arg("firefox")
            .arg(format!("https://www.youtube.com/watch?v={}", video_id))
            .output();

        let try_chrome = match res_firefox {
            Ok(output) if output.status.success() => {
                log::info!("  ✅ yt-dlp download with Firefox cookies successful.");
                false
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!(
                    "  ⚠️ yt-dlp with Firefox cookies failed (status {}): {}",
                    output.status,
                    stderr.trim()
                );
                true
            }
            Err(e) => {
                log::warn!("  ⚠️ Failed to execute yt-dlp: {}", e);
                true
            }
        };

        let try_no_cookies = if try_chrome {
            // Strategy 2: Try yt-dlp with Chrome cookies
            log::info!("  Attempting yt-dlp with Chrome cookies...");
            let res_chrome = std::process::Command::new(yt_dlp_path)
                .arg("-f")
                .arg(quality)
                .arg("-o")
                .arg(&output_str)
                .arg("--cookies-from-browser")
                .arg("chrome")
                .arg(format!("https://www.youtube.com/watch?v={}", video_id))
                .output();

            match res_chrome {
                Ok(output) if output.status.success() => {
                    log::info!("  ✅ yt-dlp download with Chrome cookies successful.");
                    false
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::warn!(
                        "  ⚠️ yt-dlp with Chrome cookies failed (status {}): {}",
                        output.status,
                        stderr.trim()
                    );
                    true
                }
                Err(e) => {
                    log::warn!("  ⚠️ Failed to execute yt-dlp for Chrome: {}", e);
                    true
                }
            }
        } else {
            false
        };

        if try_no_cookies {
            // Strategy 3: Try yt-dlp without cookies
            log::info!("  Attempting yt-dlp without cookies...");
            let res_none = std::process::Command::new(yt_dlp_path)
                .arg("-f")
                .arg(quality)
                .arg("-o")
                .arg(&output_str)
                .arg(format!("https://www.youtube.com/watch?v={}", video_id))
                .output();

            match res_none {
                Ok(output) if output.status.success() => {
                    log::info!("  ✅ yt-dlp download without cookies successful.");
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    log::error!(
                        "  ❌ All download strategies failed. yt-dlp without cookies returned {}: {}",
                        output.status,
                        stderr.trim()
                    );
                    return None;
                }
                Err(e) => {
                    log::error!(
                        "  ❌ All download strategies failed. Failed to execute yt-dlp: {}",
                        e
                    );
                    return None;
                }
            }
        }
    }

    // Find the video file by scanning the directory (in case yaydl/yt-dlp changed extension)
    let video_files: Option<Vec<PathBuf>> = std::fs::read_dir(video_dir).ok().map(|dir| {
        dir.filter_map(|e| e.ok())
            .filter(|e| {
                if let Some(ext) = e.path().extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    matches!(
                        ext_str.as_str(),
                        "mp4" | "webm" | "avi" | "mkv" | "mov" | "mpg" | "flv"
                    )
                } else {
                    false
                }
            })
            .map(|e| e.path().clone())
            .collect()
    });

    video_files.and_then(|v| v.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::test_util::tmp_dir;

    /// `ytt` falso: guarda sus argumentos en `args`, escribe `texto` en el `-o` y sale con `code`.
    fn fake_ytt(dir: &Path, texto: &str, code: i32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("ytt");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\n\
                 printf '{texto}' > \"$out\"\nexit {code}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    // Auditoría #5: si ytt falla es un error, y no queda un archivo parcial que la caché acepte.
    #[test]
    fn ytt_falla_es_error_sin_cache_parcial() {
        let dir = tmp_dir("ytt-falla");
        let dest = dir.join("transcript.txt");
        let r = fetch_transcript(&fake_ytt(&dir, "parcial", 1), "abc", &["es"], &dest);
        assert!(r.is_err(), "ytt falló: no es 'sin subtítulos'");
        assert!(!dest.exists(), "la caché no debe ver un archivo parcial");
    }

    // Auditoría #5: un ID que empieza con '-' va después de `--`, no como flag.
    #[test]
    fn id_con_guion_va_despues_de_doble_guion() {
        let dir = tmp_dir("ytt-guion");
        let dest = dir.join("transcript.txt");
        fetch_transcript(&fake_ytt(&dir, "hola", 0), "-abc123", &["es"], &dest).unwrap();
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(&args[args.len() - 2..], ["--", "-abc123"]);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hola");
    }
}
