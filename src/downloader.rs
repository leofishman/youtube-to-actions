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
    fallback: &AudioFallback,
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
        if !fetch_transcript("ytt", video_id, subtitle_langs, &transcript_path)? {
            // Sin subtítulos: único caso en que se baja audio (riesgo de baneo, ver README)
            transcribe_audio_fallback(
                yt_dlp_path,
                video_id,
                &video_dir,
                fallback,
                &transcript_path,
            )?;
        }
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

/// Config del respaldo para videos sin subtítulos
#[derive(Debug, Clone, Default)]
pub struct AudioFallback {
    pub cookies_from_browser: Option<String>,
    pub asr_command: Option<String>,
}

const SIN_SUBTITULOS: &str =
    "el video no tiene subtítulos todavía; reintentá en unas horas o configurá asr_command";

/// Baja la transcripción con `ytt` (o el programa que se pase, para tests) a `dest`.
/// `Ok(true)` = bajó; `Ok(false)` = el video no tiene subtítulos (único caso que habilita el
/// respaldo de audio). Cualquier otro fallo de `ytt` es error. `ytt` escribe a un `.part.txt` que se
/// renombra solo si terminó bien: la caché nunca ve un archivo a medias.
fn fetch_transcript(ytt: &str, video_id: &str, langs: &[&str], dest: &Path) -> Result<bool> {
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
        let low = sub_stderr.to_lowercase();
        // Mensajes de ytt: "Transcripts disabled for video" / "No transcript found for video"
        if low.contains("transcripts disabled") || low.contains("no transcript found") {
            return Ok(false);
        }
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
    Ok(true)
}

/// Respaldo: audio con yt-dlp (una sola descarga, cacheada) + ASR externo → `dest`.
fn transcribe_audio_fallback(
    yt_dlp: &str,
    video_id: &str,
    video_dir: &Path,
    fb: &AudioFallback,
    dest: &Path,
) -> Result<()> {
    let Some(asr) = fb.asr_command.as_deref() else {
        anyhow::bail!("{SIN_SUBTITULOS}");
    };
    let audio = match find_audio(video_dir) {
        Some(a) => {
            log::info!("  🎧 Audio cache hit: {a:?}");
            a
        }
        None => fetch_audio(
            yt_dlp,
            video_id,
            video_dir,
            fb.cookies_from_browser.as_deref(),
        )?,
    };
    log::info!("  🗣️ Sin subtítulos: transcribiendo {audio:?} con asr_command...");
    run_asr(asr, &audio, dest)
}

/// Audio ya descargado (`audio.<ext>`, sin restos `.part`/`.ytdl`)
fn find_audio(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_stem().is_some_and(|s| s == "audio")
                && p.extension()
                    .is_some_and(|e| e != "part" && e != "ytdl" && e != "wav")
        })
}

/// Baja SOLO el audio, una vez, sin reintentos agresivos. Si YouTube limita o pide captcha
/// se corta (reintentar empeora el baneo de IP). Un live en curso/recién terminado no se baja.
fn fetch_audio(yt_dlp: &str, video_id: &str, dir: &Path, cookies: Option<&str>) -> Result<PathBuf> {
    let mut cmd = std::process::Command::new(yt_dlp);
    cmd.args(["-f", "bestaudio", "--no-playlist", "--limit-rate", "2M"])
        .args([
            "--sleep-requests",
            "1",
            "--retries",
            "2",
            "--fragment-retries",
            "2",
        ])
        .args(["--extractor-retries", "1", "--abort-on-error"])
        // live en curso o recién terminado (aún procesándose): yt-dlp lo saltea, no baja infinito
        .args(["--match-filters", "!is_live & live_status != post_live"]);
    if let Some(b) = cookies {
        cmd.args(["--cookies-from-browser", b]);
    }
    let out = cmd
        .arg("-o")
        .arg(dir.join("audio.%(ext)s"))
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .output()
        .with_context(|| format!("No se pudo ejecutar {yt_dlp}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let low = stderr.to_lowercase();
    if ["429", "too many requests", "captcha", "sign in to confirm"]
        .iter()
        .any(|k| low.contains(k))
    {
        anyhow::bail!(
            "YouTube limitó o bloqueó a yt-dlp ({}): no se reintenta para no empeorar el baneo de IP. \
             Esperá unas horas o probá yt_dlp_cookies_from_browser",
            stderr.lines().last().unwrap_or("").trim()
        );
    }
    let live =
        low.contains("live") || low.contains("premiere") || low.contains("does not pass filter");
    if let Some(a) = find_audio(dir) {
        return Ok(a);
    }
    if live {
        anyhow::bail!(
            "el video es un live en curso, un estreno o terminó hace poco y no tiene audio disponible todavía; reintentá más tarde"
        );
    }
    anyhow::bail!(
        "yt-dlp no dejó audio para {video_id} ({}): {}",
        out.status,
        stderr.trim()
    )
}

/// Ejecuta `asr_command` (plantilla con `{audio}` y opcional `{out}`) y deja el texto en `dest`
fn run_asr(template: &str, audio: &Path, dest: &Path) -> Result<()> {
    let q = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let part = dest.with_extension("part.txt");
    let use_out = template.contains("{out}");
    let cmd = template
        .replace("{audio}", &q(audio))
        .replace("{out}", &q(&part));
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .context("No se pudo ejecutar asr_command")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!(
            "asr_command falló ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if !use_out {
        std::fs::write(&part, &out.stdout)?;
    }
    let texto = std::fs::read_to_string(&part).unwrap_or_default();
    if texto.trim().is_empty() {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("asr_command no produjo texto para {audio:?}");
    }
    std::fs::rename(&part, dest).with_context(|| format!("No se pudo mover {part:?} a {dest:?}"))
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

    /// Script falso `name` en `dir` con el cuerpo `sh` dado
    fn fake(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join(name);
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    /// yt-dlp falso: cuenta llamadas en `calls`, guarda args en `args` y crea `audio.m4a` en `-o`
    fn fake_ytdlp_ok(dir: &Path) -> String {
        fake(
            dir,
            "yt-dlp",
            "d=$(dirname \"$0\"); echo x >> \"$d/calls\"; printf '%s\\n' \"$@\" > \"$d/args\"\n\
             while [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\n\
             printf 'hola desde el audio' > \"${out%.%(ext)s}.m4a\"",
        )
    }

    fn fb(asr: Option<&str>) -> AudioFallback {
        AudioFallback {
            cookies_from_browser: None,
            asr_command: asr.map(String::from),
        }
    }

    #[test]
    fn ytt_sin_subtitulos_es_ok_false_y_otro_error_sigue_siendo_error() {
        let dir = tmp_dir("ytt-sinsub");
        let dest = dir.join("transcript.txt");
        let ytt = fake(
            &dir,
            "ytt",
            "echo 'Transcripts disabled for video: abc' >&2; exit 1",
        );
        assert!(!fetch_transcript(&ytt, "abc", &["es"], &dest).unwrap());
        let ytt = fake(&dir, "ytt2", "echo 'connection reset' >&2; exit 1");
        assert!(fetch_transcript(&ytt, "abc", &["es"], &dest).is_err());
    }

    #[test]
    fn respaldo_sin_asr_command_dice_que_configurar() {
        let dir = tmp_dir("fb-sinasr");
        let yt = fake_ytdlp_ok(&dir);
        let e = transcribe_audio_fallback(&yt, "abc", &dir, &fb(None), &dir.join("transcript.txt"))
            .unwrap_err();
        assert!(format!("{e}").contains("configurá asr_command"));
        assert!(
            !dir.join("calls").exists(),
            "sin asr no se debe tocar YouTube"
        );
    }

    #[test]
    fn respaldo_baja_audio_una_vez_con_limites_y_reusa_cache() {
        let dir = tmp_dir("fb-ok");
        let dest = dir.join("transcript.txt");
        let yt = fake_ytdlp_ok(&dir);
        let mut f = fb(Some("cat {audio}"));
        f.cookies_from_browser = Some("firefox".into());
        transcribe_audio_fallback(&yt, "abc", &dir, &f, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "hola desde el audio"
        );
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        for a in [
            "--limit-rate",
            "--sleep-requests",
            "--retries",
            "firefox",
            "--no-playlist",
        ] {
            assert!(args.contains(a), "falta {a}");
        }
        // transcript.txt perdido: el audio cacheado evita una segunda descarga
        std::fs::remove_file(&dest).unwrap();
        transcribe_audio_fallback(&yt, "abc", &dir, &f, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn asr_con_out_lee_el_archivo() {
        let dir = tmp_dir("fb-out");
        let dest = dir.join("transcript.txt");
        let yt = fake_ytdlp_ok(&dir);
        transcribe_audio_fallback(
            &yt,
            "abc",
            &dir,
            &fb(Some("tr a-z A-Z < {audio} > {out}")),
            &dest,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "HOLA DESDE EL AUDIO"
        );
    }

    #[test]
    fn asr_que_falla_o_sin_texto_es_error_sin_cache() {
        let dir = tmp_dir("fb-asrmal");
        let dest = dir.join("transcript.txt");
        let yt = fake_ytdlp_ok(&dir);
        assert!(transcribe_audio_fallback(&yt, "abc", &dir, &fb(Some("exit 3")), &dest).is_err());
        assert!(transcribe_audio_fallback(&yt, "abc", &dir, &fb(Some("true")), &dest).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn ytdlp_429_corta_sin_reintentar() {
        let dir = tmp_dir("fb-429");
        let yt = fake(
            &dir,
            "yt-dlp",
            "echo x >> \"$(dirname \"$0\")/calls\"; echo 'HTTP Error 429: Too Many Requests' >&2; exit 1",
        );
        let e = transcribe_audio_fallback(
            &yt,
            "abc",
            &dir,
            &fb(Some("cat {audio}")),
            &dir.join("transcript.txt"),
        )
        .unwrap_err();
        assert!(format!("{e}").contains("baneo"));
        assert_eq!(
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn video_live_sin_audio_es_error_claro() {
        let dir = tmp_dir("fb-live");
        let yt = fake(
            &dir,
            "yt-dlp",
            "echo 'abc does not pass filter !is_live, skipping' >&2; exit 0",
        );
        let e = transcribe_audio_fallback(
            &yt,
            "abc",
            &dir,
            &fb(Some("cat {audio}")),
            &dir.join("transcript.txt"),
        )
        .unwrap_err();
        assert!(format!("{e}").contains("live"));
    }
}
