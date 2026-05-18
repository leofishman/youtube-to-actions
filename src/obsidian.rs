use crate::types::ProcessedVideo;
use anyhow::{Context, Result};
use std::path::Path;

/// Create an Obsidian note from a processed video, placed in the folder chosen by the AI
pub fn create_note(
    vault_path: &Path,
    processed: &ProcessedVideo,
) -> Result<std::path::PathBuf> {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let filename = format!("{}.md", slugify(&processed.video.title));
    let dir = vault_path.join(&processed.target_folder);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create directory: {:?}", dir))?;

    let path = dir.join(&filename);

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

    // Escape title for YAML (replace " with \")
    let escaped_title = processed.video.title.replace('"', "\\\"");

    let mut content = format!(
        r#"---
title: "{escaped_title}"
url: https://youtube.com/watch?v={id}
channel: "{channel}"
duration: {duration}
date_added: {date}
category: {category}
tags: [{tags}]
---

# {title}

![Poster](https://img.youtube.com/vi/{id}/maxresdefault.jpg)

**Canal:** {channel}
**Duración:** {duration}
**Link:** https://youtube.com/watch?v={id}
{local_link}

## Resumen

{summary}

## Puntos Clave

{key_points}
"#,
        escaped_title = escaped_title,
        title = processed.video.title,
        id = processed.video.id,
        channel = processed.video.channel,
        duration = duration,
        date = date,
        category = format!("{:?}", processed.classification.category),
        tags = processed
            .classification
            .tags
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", "),
        local_link = local_link,
        summary = processed.summary,
        key_points = processed
            .key_points
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // Append Fabric pattern analysis if available
    if let Some(ref fabric_output) = processed.fabric_output {
        let cleaned = fabric_output.replace("INPUT:\n", "").replace("INPUT:", "");
        content.push_str(&format!("\n## Análisis Profundo\n\n{cleaned}\n"));
    }

    // Transcript section
    let transcript_text = match processed.transcript.as_ref() {
        Some(t) => t
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        None => "No disponible — el video no tenía subtítulos.".to_string(),
    };

    content.push_str(&format!("\n## Transcripción\n\n{}\n", transcript_text));

    std::fs::write(&path, &content)
        .with_context(|| format!("Failed to write note: {:?}", path))?;

    log::info!("Created Obsidian note: {:?}", path);
    Ok(path)
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
