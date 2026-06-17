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
    let filename = format!("{}.md", slugify(&processed.video.title));
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
    let path = dir.join(&filename);

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
    let escaped_category = crate::security::escape_yaml(&format!("{:?}", processed.classification.category));
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
