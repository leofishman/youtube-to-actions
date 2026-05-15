use crate::types::{Classification, ProcessedVideo, SuggestedAction, Video, VideoCategory};
use anyhow::Result;
use serde_json::Value;

/// AI processor: takes a video + transcript, returns classification + summary + target folder
pub struct Processor {
    client: reqwest::Client,
    ollama_url: String,
    ollama_model: String,
    /// List of valid vault folders the AI can choose from
    valid_folders: Vec<String>,
}

impl Processor {
    pub fn new(ollama_url: &str, ollama_model: &str, valid_folders: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            ollama_url: ollama_url.trim_end_matches('/').to_string(),
            ollama_model: ollama_model.to_string(),
            valid_folders,
        }
    }

    /// Process a video through the LLM to get summary, key points, classification, and folder
    pub async fn process(&self, video: &Video, transcript: Option<&str>) -> Result<ProcessedVideo> {
        let prompt = self.build_prompt(video, transcript);
        let response = self.call_ollama(&prompt).await?;
        let parsed = self.parse_response(&response);

        Ok(ProcessedVideo {
            video: video.clone(),
            transcript: transcript.map(|s| s.to_string()),
            summary: parsed.summary,
            key_points: parsed.key_points,
            classification: parsed.classification,
            target_folder: parsed.target_folder,
        })
    }

    fn build_prompt(&self, video: &Video, transcript: Option<&str>) -> String {
        let duration = match video.duration_seconds {
            Some(s) if s > 3600 => format!("{:.1}h", s as f64 / 3600.0),
            Some(s) if s > 60 => format!("{}m", s / 60),
            Some(s) => format!("{}s", s),
            None => "unknown".to_string(),
        };

        let folders = self.valid_folders.join(" / ");

        let mut prompt = format!(
            r#"Analyze this YouTube video and return a JSON object.

Title: {title}
Channel: {channel}
Duration: {duration}
Description: {description}
"#,
            title = video.title,
            channel = video.channel,
            duration = duration,
            description = video.description
        );

        if let Some(transcript) = transcript {
            prompt.push_str(&format!(
                "\nTranscript (first 8000 chars):\n{}\n",
                &transcript[..transcript.len().min(8000)]
            ));
        }

        prompt.push_str(&format!(
            r#"
Respond with ONLY valid JSON (no markdown, no code fences):

{{
  "summary": "2-3 paragraph summary in Spanish",
  "key_points": ["point 1", "point 2", "point 3"],
  "category": "tutorial|news|concept|entertainment|tool|health|other",
  "tags": ["tag1", "tag2"],
  "suggested_action": "watch_full|read_transcript|save_for_later|archive",
  "target_folder": "one of: {folders}"
}}

Rules:
- summary: In Spanish, concise but informative
- key_points: 3-7 bullet points of actionable takeaways
- category: tutorial=how-to, news=current events, concept=theoretical, entertainment=fun, tool=software/product, health=wellness
- tags: Short keywords relevant to content (e.g. ["rust", "api", "backend"])
- suggested_action: archive=just note it, read_transcript=summary enough, watch_full=need to see it, save_for_later=interesting but not urgent
- target_folder: Choose the BEST folder based on the video CONTENT, not just its category. Read the transcript and decide where this fits best in the vault structure.
"#,
            folders = folders
        ));

        prompt
    }

    async fn call_ollama(&self, prompt: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": self.ollama_model,
            "prompt": prompt,
            "stream": false,
            "format": "json",
        });

        let resp = self
            .client
            .post(format!("{}/api/generate", self.ollama_url))
            .json(&body)
            .send()
            .await?
            .json::<Value>()
            .await?;

        Ok(resp["response"].as_str().unwrap_or("{}").to_string())
    }

    fn parse_response(&self, raw: &str) -> ParsedOutput {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                let start = raw.find('{');
                let end = raw.rfind('}');
                match (start, end) {
                    (Some(s), Some(e)) => {
                        serde_json::from_str(&raw[s..=e]).unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                }
            }
        };

        let summary = v["summary"]
            .as_str()
            .unwrap_or("No summary generated.")
            .to_string();

        let key_points: Vec<String> = v["key_points"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();

        let category = match v["category"].as_str().unwrap_or("other") {
            "tutorial" => VideoCategory::Tutorial,
            "news" => VideoCategory::News,
            "concept" => VideoCategory::Concept,
            "entertainment" => VideoCategory::Entertainment,
            "tool" => VideoCategory::Tool,
            "health" => VideoCategory::Health,
            other => VideoCategory::Other(other.to_string()),
        };

        let action = match v["suggested_action"].as_str().unwrap_or("save_for_later") {
            "watch_full" => SuggestedAction::WatchFull,
            "read_transcript" => SuggestedAction::ReadTranscript,
            "save_for_later" => SuggestedAction::SaveForLater,
            "archive" => SuggestedAction::Archive,
            _ => SuggestedAction::SaveForLater,
        };

        let tags: Vec<String> = v["tags"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();

        // AI-suggested folder — validate it's in our list
        let target_folder = v["target_folder"]
            .as_str()
            .map(|s| s.to_string())
            .filter(|f| self.valid_folders.iter().any(|vf| vf == f))
            .unwrap_or_else(|| {
                // Fallback: use the category-to-folder mapping
                match category {
                    VideoCategory::Tutorial => "Learn",
                    VideoCategory::Concept => "Ideas",
                    VideoCategory::Tool => "Resources",
                    VideoCategory::News => "Resources",
                    VideoCategory::Health => "Health",
                    VideoCategory::Entertainment => "Things",
                    VideoCategory::Other(_) => "Resources/YouTube",
                }
                .to_string()
            });

        ParsedOutput {
            summary,
            key_points,
            target_folder,
            classification: Classification {
                category,
                tags,
                suggested_action: action,
            },
        }
    }
}

struct ParsedOutput {
    summary: String,
    key_points: Vec<String>,
    target_folder: String,
    classification: Classification,
}
