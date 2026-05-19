use crate::types::ProcessedVideo;
use anyhow::{Context, Result};
use std::path::Path;

/// Create an Obsidian note from a processed video, placed in the folder chosen by the AI
pub fn create_note(
    vault_path: &Path,
    processed: &ProcessedVideo,
    folder_override: Option<&str>,
) -> Result<std::path::PathBuf> {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let filename = format!("{}.md", slugify(&processed.video.title));
    let folder = folder_override.unwrap_or(&processed.target_folder);
    let dir = vault_path.join(folder);
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

    let mut content = format!(
        r#"---
title: "{escaped_title}"
url: https://youtube.com/watch?v={id}
channel: "{channel}"
duration: {duration}
published: {published_at}
date_added: {date}
category: {category}
tags: [{tags}]
views: {views}
likes: {likes}
dislikes: {dislikes}
comments: {comments}
---

# {title}

[![Poster](https://img.youtube.com/vi/{id}/maxresdefault.jpg)](https://youtube.com/watch?v={id})

**Canal:** {channel}
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
        title = processed.video.title,
        id = processed.video.id,
        channel = processed.video.channel,
        duration = duration,
        published_at = published_at,
        date = date,
        category = format!("{:?}", processed.classification.category),
        tags = processed
            .classification
            .tags
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", "),
        views = views,
        likes = likes,
        dislikes = dislikes,
        comments = comments,
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
