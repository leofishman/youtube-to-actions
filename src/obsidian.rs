use crate::types::{ProcessedVideo, VideoCategory};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default folder for each category (used when no override in config)
fn default_category_folders() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    m.insert("tutorial", "Learn");
    m.insert("concept", "Ideas");
    m.insert("tool", "Resources");
    m.insert("news", "Resources");
    m.insert("health", "Health");
    m.insert("entertainment", "Things");
    m.insert("other", "Resources/YouTube");
    m
}

/// Resolve the vault folder for a category, checking user overrides first
fn resolve_folder(
    category: &VideoCategory,
    overrides: &HashMap<String, String>,
) -> String {
    let key = match category {
        VideoCategory::Tutorial => "tutorial",
        VideoCategory::Concept => "concept",
        VideoCategory::Tool => "tool",
        VideoCategory::News => "news",
        VideoCategory::Health => "health",
        VideoCategory::Entertainment => "entertainment",
        VideoCategory::Other(_) => "other",
    };

    // User override wins
    if let Some(folder) = overrides.get(key) {
        return folder.clone();
    }

    // Fall back to default
    default_category_folders()
        .get(key)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Resources/YouTube".to_string())
}

/// Create an Obsidian note from a processed video, placed in the correct category folder
///
/// `overrides` is an optional mapping of category → vault folder, from config.toml
pub fn create_note(
    vault_path: &PathBuf,
    processed: &ProcessedVideo,
    overrides: &HashMap<String, String>,
) -> Result<PathBuf> {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let slug = slugify(&processed.video.title);
    let folder = resolve_folder(&processed.classification.category, overrides);
    let filename = format!("{date} - {slug}.md");
    let dir = vault_path.join(&folder);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create directory: {:?}", dir))?;

    let path = dir.join(&filename);

    let duration = match processed.video.duration_seconds {
        Some(s) if s > 3600 => format!("{:.1}h", s as f64 / 3600.0),
        Some(s) if s > 60 => format!("{}m", s / 60),
        Some(s) => format!("{}s", s),
        None => "unknown".to_string(),
    };

    let content = format!(
        r#"---
title: "{title}"
url: https://youtube.com/watch?v={id}
channel: "{channel}"
duration: {duration}
date_added: {date}
category: {category}
tags: [{tags}]
---

# {title}

**Canal:** {channel}
**Duración:** {duration}
**Link:** https://youtube.com/watch?v={id}

## Resumen

{summary}

## Puntos Clave

{key_points}

## Transcripción

> {transcript_note}
"#,
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
        summary = processed.summary,
        key_points = processed
            .key_points
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
        transcript_note = match processed.transcript {
            Some(_) => "Disponible en el procesamiento original.",
            None => "No disponible para este video.",
        },
    );

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
            'a'..='z' | '0'..='9' | '-' | '_' => c,
            ' ' | '/' | '\\' | ':' | '.' | ',' | '!' | '?' => '-',
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_work() {
        let empty = HashMap::new();
        assert_eq!(resolve_folder(&VideoCategory::Tutorial, &empty), "Learn");
        assert_eq!(resolve_folder(&VideoCategory::Health, &empty), "Health");
        assert_eq!(resolve_folder(&VideoCategory::Other("x".into()), &empty), "Resources/YouTube");
    }

    #[test]
    fn test_overrides_win() {
        let mut overrides = HashMap::new();
        overrides.insert("tutorial".to_string(), "Knowledge/Tutorials".to_string());
        assert_eq!(resolve_folder(&VideoCategory::Tutorial, &overrides), "Knowledge/Tutorials");
    }
}
