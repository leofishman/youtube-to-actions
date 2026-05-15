use crate::types::Video;
use anyhow::{Context, Result};
use hyper_util::client::legacy::connect::HttpConnector;
use serde_json::Value;
use std::path::Path;
use yup_oauth2::authenticator::Authenticator;
use yup_oauth2::hyper_rustls;

/// Authenticated YouTube Data API v3 client using OAuth2
pub struct YoutubeClient {
    auth: Authenticator<hyper_rustls::HttpsConnector<HttpConnector>>,
    client: reqwest::Client,
}

impl YoutubeClient {
    /// Build an authenticated client from credentials.json.
    ///
    /// Opens browser for consent on first run, caches token in token_cache.json.
    pub async fn authenticate(credentials_path: &Path) -> Result<Self> {
        let secret = yup_oauth2::read_application_secret(credentials_path)
            .await
            .context("Failed to read credentials.json")?;

        let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
            secret,
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
        .persist_tokens_to_disk("token_cache.json")
        .build()
        .await
        .context("Failed to build OAuth authenticator")?;

        log::info!("OAuth2 ready (token cached in token_cache.json)");

        Ok(Self {
            auth,
            client: reqwest::Client::new(),
        })
    }

    async fn get_token(&self) -> Result<String> {
        let scopes = &["https://www.googleapis.com/auth/youtube"];
        let token = self
            .auth
            .token(scopes)
            .await
            .context("OAuth failed")?;
        token
            .token()
            .map(|s| s.to_string())
            .context("Empty OAuth token")
    }

    pub async fn get_playlist_videos(&self, playlist_id: &str) -> Result<Vec<Video>> {
        let token = self.get_token().await?;
        let mut videos = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://www.googleapis.com/youtube/v3/playlistItems?part=snippet,contentDetails&maxResults=50&playlistId={playlist_id}"
            );
            if let Some(ref t) = page_token {
                url.push_str(&format!("&pageToken={t}"));
            }

            let resp = self
                .client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .context("YouTube API request failed")?
                .json::<Value>()
                .await
                .context("Failed to parse API response")?;

            if let Some(error) = resp.get("error") {
                anyhow::bail!(
                    "YouTube API error: {}",
                    error["message"].as_str().unwrap_or("unknown")
                );
            }

            if let Some(items) = resp["items"].as_array() {
                for item in items {
                    let snippet = &item["snippet"];
                    let content = &item["contentDetails"];
                    let video_id = content["videoId"].as_str().unwrap_or("").to_string();
                    if video_id.is_empty() {
                        continue;
                    }

                    // Capture the playlist item ID for later removal
                    let playlist_item_id = item["id"].as_str().unwrap_or("").to_string();

                    videos.push(Video {
                        id: video_id,
                        playlist_item_id,
                        title: snippet["title"].as_str().unwrap_or("").to_string(),
                        channel: snippet["videoOwnerChannelTitle"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        description: snippet["description"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        published_at: snippet["publishedAt"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        duration_seconds: None,
                    });
                }
            }

            page_token = resp["nextPageToken"].as_str().map(|s| s.to_string());
            if page_token.is_none() {
                break;
            }
        }

        self.fill_durations(&token, &mut videos).await?;
        Ok(videos)
    }

    /// Add a video to a playlist by video ID
    pub async fn add_to_playlist(&self, playlist_id: &str, video_id: &str) -> Result<String> {
        let token = self.get_token().await?;

        let body = serde_json::json!({
            "snippet": {
                "playlistId": playlist_id,
                "resourceId": {
                    "kind": "youtube#video",
                    "videoId": video_id
                }
            }
        });

        let resp = self
            .client
            .post("https://www.googleapis.com/youtube/v3/playlistItems?part=snippet")
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .context("Failed to add video to playlist")?
            .json::<Value>()
            .await?;

        if let Some(error) = resp.get("error") {
            anyhow::bail!(
                "Failed to add to playlist: {}",
                error["message"].as_str().unwrap_or("unknown")
            );
        }

        let new_item_id = resp["id"]
            .as_str()
            .context("No playlist item ID returned")
            .map(|s| s.to_string())?;

        log::info!("Added video {video_id} to playlist {playlist_id} (item: {new_item_id})");
        Ok(new_item_id)
    }

    /// Remove a video from a playlist by playlist item ID
    pub async fn remove_from_playlist(&self, playlist_item_id: &str) -> Result<()> {
        let token = self.get_token().await?;

        let url = format!(
            "https://www.googleapis.com/youtube/v3/playlistItems?id={playlist_item_id}"
        );

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .context("Failed to remove video from playlist")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to remove from playlist: HTTP {status} — {body}");
        }

        log::info!("Removed playlist item {playlist_item_id}");
        Ok(())
    }

    async fn fill_durations(&self, token: &str, videos: &mut Vec<Video>) -> Result<()> {
        for chunk in videos.chunks_mut(50) {
            let ids: Vec<&str> = chunk.iter().map(|v| v.id.as_str()).collect();
            let url = format!(
                "https://www.googleapis.com/youtube/v3/videos?part=contentDetails&id={}",
                ids.join(",")
            );

            let resp = self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .context("Duration request failed")?
                .json::<Value>()
                .await?;

            if let Some(items) = resp["items"].as_array() {
                for item in items {
                    let vid = item["id"].as_str().unwrap_or("");
                    let d = item["contentDetails"]["duration"]
                        .as_str()
                        .unwrap_or("PT0S");
                    if let Some(v) = chunk.iter_mut().find(|v| v.id == vid) {
                        v.duration_seconds = Some(parse_iso8601_duration(d));
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_iso8601_duration(duration: &str) -> u64 {
    let mut s: u64 = 0;
    let mut n = String::new();
    let mut t = false;
    for c in duration.chars() {
        match c {
            'T' => t = true,
            '0'..='9' => n.push(c),
            'H' if t => { s += n.parse::<u64>().unwrap_or(0) * 3600; n.clear(); }
            'M' if t => { s += n.parse::<u64>().unwrap_or(0) * 60; n.clear(); }
            'S' if t => { s += n.parse::<u64>().unwrap_or(0); n.clear(); }
            'D' => { s += n.parse::<u64>().unwrap_or(0) * 86400; n.clear(); }
            'P' => n.clear(),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_duration() {
        assert_eq!(parse_iso8601_duration("PT1H30M15S"), 5415);
        assert_eq!(parse_iso8601_duration("PT5M"), 300);
    }
}
