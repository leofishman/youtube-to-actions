use crate::types::ProcessedVideo;
use anyhow::{Context, Result};
use std::path::Path;

/// Create an Obsidian note from a processed video, placed in the folder chosen by the AI
pub fn create_note(
    vault_path: &Path,
    processed: &ProcessedVideo,
    folder_override: Option<&str>,
    dry_run: bool,
) -> Result<std::path::PathBuf> {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let filename = note_filename(&processed.video.title, &processed.video.id);
    let raw_folder = folder_override.unwrap_or(&processed.target_folder);

    // Defense-in-depth: reject path traversal attempts in target_folder
    let folder = if raw_folder.contains("..") || raw_folder.starts_with('/') {
        log::warn!(
            "Suspicious target_folder detected: '{}'. Using fallback 'Resources/YouTube'.",
            raw_folder
        );
        "Resources/YouTube"
    } else {
        raw_folder
    };
    let dir = vault_path.join(folder);
    // Un reproceso reescribe la nota existente del video, aunque tenga el nombre de antes del ID
    let path = find_note_of_video(&dir, &processed.video.id).unwrap_or_else(|| dir.join(&filename));

    if dry_run {
        log::info!("  📝 [Simulado] Note would be created at: {:?}", path);
        return Ok(path);
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create directory: {:?}", dir))?;

    let duration = match processed.video.duration_seconds {
        Some(s) if s > 3600 => format!("{:.1}h", s as f64 / 3600.0),
        Some(s) if s > 60 => format!("{}m", s / 60),
        Some(s) => format!("{}s", s),
        None => "unknown".to_string(),
    };

    // Build local video link (Obsidian-compatible file:// or markdown link)
    let local_link = processed
        .local_video_path
        .as_ref()
        .map(|p| format!("**Video local:** `{}`", p))
        .unwrap_or_default();

    // Escape variables for YAML frontmatter
    let escaped_title = crate::security::escape_yaml(&processed.video.title);
    let escaped_channel = crate::security::escape_yaml(&processed.video.channel);
    let escaped_category =
        crate::security::escape_yaml(&format!("{:?}", processed.classification.category));
    let escaped_tags = processed
        .classification
        .tags
        .iter()
        .map(|t| format!("\"{}\"", crate::security::escape_yaml(t)))
        .collect::<Vec<_>>()
        .join(", ");

    // Extract published date part (YYYY-MM-DD)
    let published_at = if processed.video.published_at.len() >= 10 {
        &processed.video.published_at[..10]
    } else {
        &processed.video.published_at
    };

    let views = processed.video.view_count.unwrap_or(0);
    let likes = processed.video.like_count.unwrap_or(0);
    let dislikes = processed.video.dislike_count.unwrap_or(0);
    let comments = processed.video.comment_count.unwrap_or(0);

    // Strip HTML from title, channel and local link to prevent malicious formatting
    let title_stripped = crate::security::strip_html(&processed.video.title);
    let channel_stripped = crate::security::strip_html(&processed.video.channel);

    let mut content = format!(
        r#"---
title: "{escaped_title}"
url: https://youtube.com/watch?v={id}
channel: "{escaped_channel}"
duration: {duration}
published: {published_at}
date_added: {date}
category: {escaped_category}
tags: [{escaped_tags}]
views: {views}
likes: {likes}
dislikes: {dislikes}
comments: {comments}
---

# {title_stripped}

[![Poster](https://img.youtube.com/vi/{id}/maxresdefault.jpg)](https://youtube.com/watch?v={id})

**Canal:** {channel_stripped}
**Duración:** {duration}
**Publicado:** {published_at}
**Link:** https://youtube.com/watch?v={id}
**Métricas:** {views} vistas | {likes} likes | {comments} comentarios
{local_link}

## Resumen

{summary}

## Puntos Clave

{key_points}
"#,
        escaped_title = escaped_title,
        escaped_channel = escaped_channel,
        escaped_category = escaped_category,
        escaped_tags = escaped_tags,
        title_stripped = title_stripped,
        id = processed.video.id,
        channel_stripped = channel_stripped,
        duration = duration,
        published_at = published_at,
        date = date,
        views = views,
        likes = likes,
        comments = comments,
        local_link = local_link,
        // Sanitize LLM-generated content to prevent indirect prompt injection
        summary = crate::security::sanitize_for_obsidian(&processed.summary),
        key_points = processed
            .key_points
            .iter()
            .map(|p| format!("- {}", crate::security::sanitize_for_obsidian(p)))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // Append Fabric pattern analysis if available
    if let Some(ref fabric_output) = processed.fabric_output {
        let cleaned = fabric_output.replace("INPUT:\n", "").replace("INPUT:", "");
        // Sanitize Fabric output for Obsidian (HTML + wikilinks + URIs + code fences)
        let cleaned_sanitized = crate::security::sanitize_for_obsidian(&cleaned);
        content.push_str(&format!("\n## Análisis Profundo\n\n{cleaned_sanitized}\n"));
    }

    // Transcript section (HTML stripped to prevent script/iframe execution in Obsidian)
    let transcript_text = match processed.transcript.as_ref() {
        Some(t) => {
            let stripped = crate::security::strip_html(t);
            stripped
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        }
        None => "No disponible — el video no tenía subtítulos.".to_string(),
    };

    content.push_str(&format!("\n## Transcripción\n\n{}\n", transcript_text));

    // No pisar una nota que no es de este video (auditoría #3). La del mismo video
    // sí se reescribe: es un reproceso.
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let own = format!("url: https://youtube.com/watch?v={}\n", processed.video.id);
        if !existing.contains(&own) {
            anyhow::bail!(
                "Ya existe {:?} y no es la nota de este video: no se sobrescribe",
                path
            );
        }
    }

    std::fs::write(&path, &content).with_context(|| format!("Failed to write note: {:?}", path))?;

    log::info!("Created Obsidian note: {:?}", path);
    Ok(path)
}

/// La nota de este video en `dir`, si ya existe (la identifica su línea `url:`).
fn find_note_of_video(dir: &Path, id: &str) -> Option<std::path::PathBuf> {
    let own = format!("url: https://youtube.com/watch?v={id}\n");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|x| x == "md")
                && std::fs::read_to_string(p).is_ok_and(|c| c.contains(&own))
        })
}

/// `<slug>-<id>.md`: el ID evita que dos títulos con el mismo slug se pisen y que un
/// título sin caracteres válidos dé `.md` (auditoría #3).
fn note_filename(title: &str, id: &str) -> String {
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let name = [slugify(title), id]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!(
        "{}.md",
        if name.is_empty() {
            "sin-titulo".into()
        } else {
            name
        }
    )
}

fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => c,
            'á' | 'é' | 'í' | 'ó' | 'ú' | 'ñ' => c, // keep common spanish chars
            _ => '-',
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::test_util::{processed_video, tmp_dir};

    // Auditoría #3: dos títulos distintos con el mismo slug no se pisan.
    #[test]
    fn titulos_distintos_no_se_pisan() {
        let vault = tmp_dir("slugs");
        let a = create_note(
            &vault,
            &processed_video("idA", "Rust Tutorial!"),
            None,
            false,
        )
        .unwrap();
        let b = create_note(
            &vault,
            &processed_video("idB", "rust tutorial?"),
            None,
            false,
        )
        .unwrap();
        assert_ne!(a, b);
        assert!(std::fs::read_to_string(&a).unwrap().contains("watch?v=idA"));
        assert!(std::fs::read_to_string(&b).unwrap().contains("watch?v=idB"));
    }

    // Auditoría #3: un título sin caracteres válidos no da un archivo `.md`.
    #[test]
    fn titulo_sin_caracteres_validos_no_da_punto_md() {
        let vault = tmp_dir("slug-vacio");
        for t in ["🔥🔥🔥", "日本語のタイトル"] {
            let p = create_note(&vault, &processed_video("idC", t), None, false).unwrap();
            assert_ne!(p.file_name().unwrap(), ".md", "{t}");
        }
    }

    // Auditoría #3: no se sobrescribe una nota existente que no es de este video.
    #[test]
    fn no_pisa_nota_de_otro_video() {
        let vault = tmp_dir("slug-ajena");
        let v = processed_video("idD", "Hola");
        let path = create_note(&vault, &v, None, true).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "nota escrita a mano").unwrap();
        assert!(create_note(&vault, &v, None, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "nota escrita a mano"
        );

        // la nota del mismo video sí se reescribe (reproceso con --force)
        std::fs::remove_file(&path).unwrap();
        create_note(&vault, &v, None, false).unwrap();
        create_note(&vault, &v, None, false).unwrap();
    }

    // Un reproceso reescribe la nota con el nombre de antes del ID, no crea otra.
    #[test]
    fn reproceso_reusa_la_nota_de_nombre_viejo() {
        let vault = tmp_dir("nota-vieja");
        let v = processed_video("idE", "Hola Mundo");
        let nueva = create_note(&vault, &v, None, true).unwrap();
        let vieja = nueva.parent().unwrap().join("hola-mundo.md");
        std::fs::create_dir_all(vieja.parent().unwrap()).unwrap();
        std::fs::write(&vieja, "url: https://youtube.com/watch?v=idE\nvieja").unwrap();
        let p = create_note(&vault, &v, None, false).unwrap();
        assert_eq!(p, vieja);
        assert!(!nueva.exists());
    }
}
