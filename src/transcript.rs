use anyhow::Result;

/// Fetch transcript for a YouTube video using the yt-transcript-rs crate
///
/// This crate is more actively maintained than the old youtube-transcript crate
/// and works with modern YouTube page structure via the InnerTube API.
///
/// Uses the YouTubeTranscriptApi which connects to YouTube's internal API
/// (not the public Data API) to fetch captions/transcripts.
pub async fn get_transcript(video_id: &str, preserve_formatting: bool) -> Result<Option<String>> {
    use yt_transcript_rs::YouTubeTranscriptApi;

    // Create API instance with default settings (no proxy, no cookies needed)
    let api = YouTubeTranscriptApi::new(None, None, None)
        .map_err(|e| anyhow::anyhow!("Failed to create YouTube transcript API: {}", e))?;

    // Fetch transcript - try English first, then Spanish, then auto-generated
    let transcript = api
        .fetch_transcript(video_id, &["en", "es", "en-US"], preserve_formatting)
        .await;

    match transcript {
        Ok(t) => {
            // Check if there's any content
            if t.text().is_empty() {
                log::warn!("Empty transcript for video {}", video_id);
                Ok(None)
            } else {
                // Return the full text
                Ok(Some(t.text().to_string()))
            }
        }
        Err(e) => {
            log::warn!(
                "No transcript for video {}: {} — video may not have captions",
                video_id,
                e
            );
            Ok(None)
        }
    }
}

/// List available transcript languages for a video
pub async fn get_available_languages(video_id: &str) -> Result<Vec<String>> {
    use yt_transcript_rs::YouTubeTranscriptApi;

    let api = YouTubeTranscriptApi::new(None, None, None)
        .map_err(|e| anyhow::anyhow!("Failed to create YouTube transcript API: {}", e))?;

    let transcript_list = api
        .list_transcripts(video_id)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list transcripts: {}", e))?;

    let languages: Vec<String> = transcript_list
        .transcripts()
        .filter_map(|t| {
            if t.language_code() != "auto" {
                Some(t.language_code().to_string())
            } else {
                None
            }
        })
        .collect();

    if languages.is_empty() {
        // Return auto-generated as fallback
        Ok(vec!["auto".to_string()])
    } else {
        Ok(languages)
    }
}
