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
        Some(download_actual_video(
            yt_dlp_path,
            video_id,
            &video_dir,
            quality,
            fallback,
        )?)
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
    /// Solo si el usuario lo configura; nunca se prueban navegadores por su cuenta
    pub cookies_from_browser: Option<String>,
    pub asr_command: Option<String>,
    /// Tope diario de ejecuciones de yt-dlp (audio de respaldo y video juntos)
    pub max_downloads_per_day: u32,
    /// Contador persistente del día (JSON); fuera del directorio de videos
    pub counter_path: PathBuf,
}

/// Cuenta una descarga en `path` (se reinicia si cambió el día) o falla si ya se llegó al tope.
/// Se cuenta ANTES de llamar a yt-dlp: un intento que falla también expone la IP.
fn reserve_download(path: &Path, max: u32, today: &str) -> Result<()> {
    let count = match std::fs::read_to_string(path) {
        Ok(c) => {
            let v: serde_json::Value = serde_json::from_str(&c).with_context(|| {
                format!("El contador de descargas {path:?} está corrupto; revisalo o borralo")
            })?;
            if v["date"] == today {
                v["count"].as_u64().unwrap_or(0) as u32
            } else {
                0
            }
        }
        Err(_) => 0,
    };
    if count >= max {
        anyhow::bail!(
            "tope diario de descargas alcanzado ({count}/{max} hoy): no se llama a yt-dlp. \
             Reintentá mañana o subí max_downloads_per_day en [storage]"
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::json!({"date": today, "count": count + 1}).to_string(),
    )?;
    std::fs::rename(&tmp, path).with_context(|| format!("No se pudo guardar {path:?}"))
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
        None => fetch_audio(yt_dlp, video_id, video_dir, fb)?,
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

/// Baja SOLO el audio (una vez, ver `run_ytdlp`)
fn fetch_audio(yt_dlp: &str, video_id: &str, dir: &Path, fb: &AudioFallback) -> Result<PathBuf> {
    run_ytdlp(
        yt_dlp,
        video_id,
        "bestaudio",
        &dir.join("audio.%(ext)s"),
        fb,
        || find_audio(dir),
    )
}

/// ÚNICO punto que ejecuta yt-dlp: un intento, límites prudentes, tope diario. Si YouTube limita
/// o pide captcha se corta (reintentar empeora el baneo de IP). Un live en curso/recién terminado
/// no se baja. `find` devuelve el archivo resultante.
fn run_ytdlp(
    yt_dlp: &str,
    video_id: &str,
    format: &str,
    out: &Path,
    fb: &AudioFallback,
    find: impl Fn() -> Option<PathBuf>,
) -> Result<PathBuf> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    reserve_download(&fb.counter_path, fb.max_downloads_per_day, &today)?;
    let mut cmd = std::process::Command::new(yt_dlp);
    cmd.args(["-f", format, "--no-playlist", "--limit-rate", "2M"])
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
    if let Some(b) = fb.cookies_from_browser.as_deref() {
        cmd.args(["--cookies-from-browser", b]);
    }
    let out = cmd
        .arg("-o")
        .arg(out)
        // `--`: un ID que empieza con '-' no se lee como flag
        .arg("--")
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
    if let Some(a) = find() {
        return Ok(a);
    }
    if live {
        anyhow::bail!(
            "el video es un live en curso, un estreno o terminó hace poco y no tiene audio disponible todavía; reintentá más tarde"
        );
    }
    anyhow::bail!(
        "yt-dlp no dejó el archivo para {video_id} ({}): {}",
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
    // Servidores compatibles con OpenAI devuelven {"text": "..."} si no se pide response_format=text
    let texto = serde_json::from_str::<serde_json::Value>(texto.trim())
        .ok()
        .and_then(|v| v["text"].as_str().map(String::from))
        .unwrap_or(texto);
    if texto.trim().is_empty() {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("asr_command no produjo texto para {audio:?}");
    }
    std::fs::write(&part, texto)?;
    std::fs::rename(&part, dest).with_context(|| format!("No se pudo mover {part:?} a {dest:?}"))
}

/// Video ya descargado (cualquier `*.mp4/webm/...` que no sea el audio de respaldo)
fn find_video(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_stem().is_some_and(|s| s != "audio")
                && p.extension().is_some_and(|e| {
                    matches!(
                        e.to_string_lossy().to_lowercase().as_str(),
                        "mp4" | "webm" | "avi" | "mkv" | "mov" | "mpg" | "flv"
                    )
                })
        })
}

/// Video completo (`--download-video`): reusa el archivo si ya está; si no, UN intento de yt-dlp
/// con los mismos límites que el audio. Sin cascada de navegadores ni otros programas.
fn download_actual_video(
    yt_dlp: &str,
    video_id: &str,
    dir: &Path,
    quality: &str,
    fb: &AudioFallback,
) -> Result<PathBuf> {
    if let Some(v) = find_video(dir) {
        log::info!("  📹 Video cache hit: {v:?}");
        return Ok(v);
    }
    run_ytdlp(
        yt_dlp,
        video_id,
        quality,
        &dir.join("video.%(ext)s"),
        fb,
        || find_video(dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::test_util::tmp_dir;

    /// `ytt` falso: guarda sus argumentos en `args`, escribe `texto` en el `-o` y sale con `code`.
    /// Escribe un script ejecutable sin ETXTBSY: otro test puede hacer fork con nuestro fd de
    /// escritura abierto, así que se prueba el exec (sin efectos) hasta que deje de fallar.
    fn write_exec(script: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;
        let content = content.replacen('\n', "\n[ -n \"$YT2_PROBE\" ] && exit 0\n", 1);
        std::fs::write(script, content).unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(script)
                .env("YT2_PROBE", "1")
                .status()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                _ => return,
            }
        }
    }

    fn fake_ytt(dir: &Path, texto: &str, code: i32) -> String {
        let script = dir.join("ytt");
        write_exec(
            &script,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\n\
                 printf '{texto}' > \"$out\"\nexit {code}\n"
            ),
        );
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
        let script = dir.join(name);
        write_exec(&script, &format!("#!/bin/sh\n{body}\n"));
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

    fn fb(dir: &Path, asr: Option<&str>) -> AudioFallback {
        AudioFallback {
            cookies_from_browser: None,
            asr_command: asr.map(String::from),
            max_downloads_per_day: 5,
            counter_path: dir.join("downloads.json"),
        }
    }

    fn calls(dir: &Path) -> usize {
        std::fs::read_to_string(dir.join("calls"))
            .map(|c| c.lines().count())
            .unwrap_or(0)
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
        let e = transcribe_audio_fallback(
            &yt,
            "abc",
            &dir,
            &fb(&dir, None),
            &dir.join("transcript.txt"),
        )
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
        let mut f = fb(&dir, Some("cat {audio}"));
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
            &fb(&dir, Some("tr a-z A-Z < {audio} > {out}")),
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
        assert!(
            transcribe_audio_fallback(&yt, "abc", &dir, &fb(&dir, Some("exit 3")), &dest).is_err()
        );
        assert!(
            transcribe_audio_fallback(&yt, "abc", &dir, &fb(&dir, Some("true")), &dest).is_err()
        );
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
            &fb(&dir, Some("cat {audio}")),
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
            &fb(&dir, Some("cat {audio}")),
            &dir.join("transcript.txt"),
        )
        .unwrap_err();
        assert!(format!("{e}").contains("live"));
    }

    #[test]
    fn video_hace_un_solo_intento_sin_cookies_automaticas_y_con_limites() {
        let dir = tmp_dir("vid-ok");
        // fake que escribe .mp4; lo que importa son los args y el conteo
        let yt_video = fake(
            &dir,
            "yt-dlp-v",
            "d=$(dirname \"$0\"); echo x >> \"$d/calls\"; printf '%s\\n' \"$@\" > \"$d/args\"\n\
             while [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=\"$2\"; shift; done\n\
             printf 'video' > \"${out%.%(ext)s}.mp4\"",
        );
        let v = download_actual_video(&yt_video, "abc", &dir, "worst", &fb(&dir, None)).unwrap();
        assert!(v.ends_with("video.mp4"));
        assert_eq!(calls(&dir), 1);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        for a in [
            "--limit-rate",
            "--sleep-requests",
            "--abort-on-error",
            "--match-filters",
        ] {
            assert!(args.contains(a), "falta {a}");
        }
        assert!(!args.contains("cookies"), "sin config no hay cookies");
        // cache hit: no vuelve a llamar ni a contar
        download_actual_video(&yt_video, "abc", &dir, "worst", &fb(&dir, None)).unwrap();
        assert_eq!(calls(&dir), 1);
        let c = std::fs::read_to_string(dir.join("downloads.json")).unwrap();
        assert!(c.contains("\"count\":1"));
    }

    #[test]
    fn video_con_429_corta_y_no_reintenta() {
        let dir = tmp_dir("vid-429");
        let yt = fake(
            &dir,
            "yt-dlp",
            "echo x >> \"$(dirname \"$0\")/calls\"; echo 'Sign in to confirm you are not a bot' >&2; exit 1",
        );
        let e = download_actual_video(&yt, "abc", &dir, "worst", &fb(&dir, None)).unwrap_err();
        assert!(format!("{e}").contains("baneo"));
        assert_eq!(calls(&dir), 1);
    }

    #[test]
    fn tope_diario_alcanzado_no_toca_ytdlp_ni_el_audio_cuenta_aparte() {
        let dir = tmp_dir("tope");
        let yt = fake_ytdlp_ok(&dir);
        let mut f = fb(&dir, Some("cat {audio}"));
        f.max_downloads_per_day = 1;
        transcribe_audio_fallback(&yt, "abc", &dir, &f, &dir.join("transcript.txt")).unwrap();
        // otro video: el tope (audio + video juntos) ya se gastó
        let dir2 = dir.join("otro");
        std::fs::create_dir_all(&dir2).unwrap();
        let e = download_actual_video(&yt, "def", &dir2, "worst", &f).unwrap_err();
        assert!(format!("{e}").contains("tope diario"));
        assert_eq!(calls(&dir), 1);
    }

    #[test]
    fn contador_se_reinicia_al_cambiar_el_dia() {
        let dir = tmp_dir("contador");
        let p = dir.join("downloads.json");
        reserve_download(&p, 2, "2026-10-28").unwrap();
        reserve_download(&p, 2, "2026-10-28").unwrap();
        assert!(reserve_download(&p, 2, "2026-10-28").is_err());
        reserve_download(&p, 2, "2026-10-29").unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"count\":1"));
    }

    #[test]
    fn asr_json_con_text_se_extrae() {
        let dir = tmp_dir("asr-json");
        let dest = dir.join("transcript.txt");
        run_asr(
            "echo '{\"text\": \"hola mundo\"}'",
            &dir.join("a.m4a"),
            &dest,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hola mundo");
    }
}
