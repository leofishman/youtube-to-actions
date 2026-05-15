use anyhow::Result;

/// Fetch transcript for a YouTube video using the youtube-transcript crate
pub async fn get_transcript(video_id: &str) -> Result<Option<String>> {
    use youtube_transcript::YoutubeBuilder;

    let url = format!("https://youtube.com/watch?v={video_id}");
    let builder = YoutubeBuilder::default();
    let yt = builder.build();

    match yt.transcript(&url).await {
        Ok(transcript) => {
            let text: Vec<String> = transcript
                .into_iter()
                .map(|t| t.text)
                .collect();
            Ok(Some(text.join(" ")))
        }
        Err(e) => {
            log::warn!("No transcript for video {}: {}", video_id, e);
            Ok(None)
        }
    }
}
