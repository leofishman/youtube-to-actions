use crate::types::Video;
use anyhow::{Context, Result};
use serde_json::Value;

/// YouTube Data API v3 client
pub struct YoutubeClient {
    api_key: String,
    client: reqwest::Client,
}

impl YoutubeClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Fetch all videos from a playlist (paginated)
    pub async fn get_playlist_videos(&self, playlist_id: &str) -> Result<Vec<Video>> {
        let mut videos = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://www.googleapis.com/youtube/v3/playlistItems?part=snippet,contentDetails&maxResults=50&playlistId={}&key={}",
                playlist_id, self.api_key
            );
            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .context("YouTube API request failed")?
                .json::<Value>()
                .await
                .context("Failed to parse YouTube API response")?;

            // Parse items
            if let Some(items) = resp["items"].as_array() {
                for item in items {
                    let snippet = &item["snippet"];
                    let content = &item["contentDetails"];

                    let video_id = content["videoId"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();

                    if video_id.is_empty() {
                        continue;
                    }

                    let video = Video {
                        id: video_id,
                        title: snippet["title"].as_str().unwrap_or("").to_string(),
                        channel: snippet["videoOwnerChannelTitle"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        description: snippet["description"].as_str().unwrap_or("").to_string(),
                        published_at: snippet["publishedAt"].as_str().unwrap_or("").to_string(),
                        duration_seconds: None, // Filled later via video details
                    };
                    videos.push(video);
                }
            }

            // Check for next page
            page_token = resp["nextPageToken"].as_str().map(|s| s.to_string());
            if page_token.is_none() {
                break;
            }
        }

        // Fetch durations in batch
        self.fill_durations(&mut videos).await?;

        Ok(videos)
    }

    /// Batch-fetch video durations from the Videos API
    async fn fill_durations(&self, videos: &mut Vec<Video>) -> Result<()> {
        for chunk in videos.chunks_mut(50) {
            let ids: Vec<&str> = chunk.iter().map(|v| v.id.as_str()).collect();
            let ids_param = ids.join(",");

            let url = format!(
                "https://www.googleapis.com/youtube/v3/videos?part=contentDetails&id={}&key={}",
                ids_param, self.api_key
            );

            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .context("YouTube duration request failed")?
                .json::<Value>()
                .await?;

            if let Some(items) = resp["items"].as_array() {
                for item in items {
                    let vid = item["id"].as_str().unwrap_or("");
                    let duration = item["contentDetails"]["duration"]
                        .as_str()
                        .unwrap_or("PT0S");

                    let seconds = parse_iso8601_duration(duration);

                    if let Some(v) = chunk.iter_mut().find(|v| v.id == vid) {
                        v.duration_seconds = Some(seconds);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Parse ISO 8601 duration like "PT1H30M15S" -> seconds
fn parse_iso8601_duration(duration: &str) -> u64 {
    let mut seconds: u64 = 0;
    let mut num = String::new();
    let mut in_time = false;

    for ch in duration.chars() {
        match ch {
            'P' | 'T' => {
                if ch == 'T' {
                    in_time = true;
                }
                num.clear();
            }
            '0'..='9' => num.push(ch),
            'H' if in_time => {
                seconds += num.parse::<u64>().unwrap_or(0) * 3600;
                num.clear();
            }
            'M' if in_time => {
                seconds += num.parse::<u64>().unwrap_or(0) * 60;
                num.clear();
            }
            'S' if in_time => {
                seconds += num.parse::<u64>().unwrap_or(0);
                num.clear();
            }
            'D' => {
                seconds += num.parse::<u64>().unwrap_or(0) * 86400;
                num.clear();
            }
            _ => {}
        }
    }
    seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_parsing() {
        assert_eq!(parse_iso8601_duration("PT1H30M15S"), 5415);
        assert_eq!(parse_iso8601_duration("PT5M"), 300);
        assert_eq!(parse_iso8601_duration("PT0S"), 0);
        assert_eq!(parse_iso8601_duration("P1DT2H"), 93600);
    }
}