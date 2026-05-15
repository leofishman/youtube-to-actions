use crate::types::{Classification, ProcessedVideo, SuggestedAction, Video, VideoCategory};
use anyhow::Result;
use serde_json::Value;

/// AI processor: takes a video + transcript, returns classification + summary + target folder
pub struct Processor {
    client: reqwest::Client,
    ollama_url: String,
    ollama_model: String,
    /// List of valid vault folders the AI can choose from (lowercase for matching)
    valid_folders: Vec<String>,
    /// Original case versions of folders
    valid_folders_pretty: Vec<String>,
}

impl Processor {
    pub fn new(ollama_url: &str, ollama_model: &str, valid_folders: Vec<String>) -> Self {
        let pretty = valid_folders.clone();
        let lower: Vec<String> = valid_folders.iter().map(|f| f.to_lowercase()).collect();
        Self {
            client: reqwest::Client::new(),
            ollama_url: ollama_url.trim_end_matches('/').to_string(),
            ollama_model: ollama_model.to_string(),
            valid_folders: lower,
            valid_folders_pretty: pretty,
        }
    }

    /// Process a video through the LLM to get summary, key points, classification, and folder
    pub async fn process(&self, video: &Video, transcript: Option<&str>) -> Result<ProcessedVideo> {
        let prompt = self.build_prompt(video, transcript);
        let response = self.call_ollama(&prompt).await?;

        log::debug!("AI raw response (first 500 chars): {}", &response[..response.len().min(500)]);

        let parsed = self.parse_response(&response);

        log::info!(
            "  🧠 AI → categoría: {:?}, carpeta: {}, acción: {:?}",
            parsed.classification.category,
            parsed.target_folder,
            parsed.classification.suggested_action,
        );
        if !parsed.classification.tags.is_empty() {
            log::info!("  🏷️  Tags: {}", parsed.classification.tags.join(", "));
        }

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

        let folders_list = self
            .valid_folders_pretty
            .iter()
            .map(|f| format!("  - \"{f}\""))
            .collect::<Vec<_>>()
            .join("\n");

        let mut prompt = format!(
            r#"Analyze this YouTube video and return a JSON object.

Title: {title}
Channel: {channel}
Duration: {duration}
Description:
{description}
"#,
            title = video.title,
            channel = video.channel,
            duration = duration,
            description = video.description
        );

        if let Some(transcript) = transcript {
            // Include more transcript content for better analysis
            let max_chars = 12000;
            let truncated = if transcript.len() > max_chars {
                format!("{}...[TRUNCATED at {} chars]", &transcript[..max_chars], max_chars)
            } else {
                transcript.to_string()
            };
            prompt.push_str(&format!(
                "\nTranscript:\n{truncated}\n"
            ));
        }

        // Build the folder constraint as an explicit list
        prompt.push_str(&format!(
            r#"
Respond with ONLY valid JSON (no markdown, no code fences):

{{
  "summary": "2-3 paragraph summary in Spanish",
  "key_points": ["point 1", "point 2", "point 3"],
  "category": "tutorial|news|concept|entertainment|tool|health|other",
  "tags": ["tag1", "tag2"],
  "suggested_action": "watch_full|read_transcript|save_for_later|archive",
  "target_folder": "EXACTLY one of the folder names listed below — pick the one that best matches the video CONTENT"
}}

AVAILABLE FOLDERS (pick EXACTLY one):
{folders_list}

RULES:
- summary: In Spanish, concise but informative
- key_points: 3-7 bullet points of actionable takeaways
- category: tutorial=how-to, news=current events, concept=theoretical/thematic, entertainment=fun, tool=software/product/service, health=wellness/nutrition/fitness/medicine
- tags: Short keywords relevant to content (e.g. ["rust", "api", "backend"])
- suggested_action: archive=just note it, read_transcript=summary enough, watch_full=need to see it, save_for_later=interesting but not urgent
- target_folder: Choose the BEST folder from the list above based on the video TITLE, DESCRIPTION, and TRANSCRIPT. Read the content and decide where this fits best.

IMPORTANT: "target_folder" must be EXACTLY one value from the AVAILABLE FOLDERS list above, with NO extra text, NO quotes around the whole thing, and NO explanation. Example: "Health" or "Learn" or "Resources/YouTube".
"#,
            folders_list = folders_list,
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

        Ok(resp["response"].as_str().unwrap_or("{ }").to_string())
    }

    fn parse_response(&self, raw: &str) -> ParsedOutput {
        let v: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("AI JSON parse error: {e}. Attempting recovery...");
                log::debug!("Raw AI response: {raw}");
                // Try to find JSON in the response (handle markdown code fences, etc.)
                let start = raw.find('{');
                let end = raw.rfind('}');
                match (start, end) {
                    (Some(s), Some(e)) if s < e => {
                        let extracted = &raw[s..=e];
                        serde_json::from_str(extracted).unwrap_or_else(|e2| {
                            log::error!("JSON recovery also failed: {e2}");
                            log::debug!("Extracted JSON attempt: {extracted}");
                            Value::Null
                        })
                    }
                    _ => {
                        log::error!("No JSON found in AI response at all");
                        Value::Null
                    }
                }
            }
        };

        if v == Value::Null {
            log::error!("AI returned no usable JSON. Returning default values.");
            return ParsedOutput::default();
        }

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

        // Case-insensitive folder matching with logging
        let ai_folder = v["target_folder"]
            .as_str()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let target_folder = if ai_folder.is_empty() {
            log::warn!("AI did not return a target_folder value");
            None
        } else {
            let ai_lower = ai_folder.to_lowercase();
            match self.valid_folders.iter().position(|f| f == &ai_lower) {
                Some(idx) => {
                    log::debug!("AI folder '{ai_folder}' matched to '{}'", self.valid_folders_pretty[idx]);
                    Some(self.valid_folders_pretty[idx].clone())
                }
                None => {
                    log::warn!(
                        "AI returned unknown folder '{ai_folder}'. Valid options: {}",
                        self.valid_folders_pretty.join(", ")
                    );
                    None
                }
            }
        };

        let target_folder = target_folder.unwrap_or_else(|| {
            // Fallback: use the category-to-folder mapping
            let fb = match category {
                VideoCategory::Tutorial => "Learn",
                VideoCategory::Concept => "Ideas",
                VideoCategory::Tool => "Resources",
                VideoCategory::News => "Resources",
                VideoCategory::Health => "Health",
                VideoCategory::Entertainment => "Things",
                VideoCategory::Other(_) => "Resources/YouTube",
            };
            log::info!("  Fallback: category {:?} → folder '{fb}'", category);
            fb.to_string()
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

impl Default for ParsedOutput {
    fn default() -> Self {
        Self {
            summary: "No se pudo generar resumen.".to_string(),
            key_points: vec![],
            target_folder: "Resources/YouTube".to_string(),
            classification: Classification {
                category: VideoCategory::Other("unknown".to_string()),
                tags: vec![],
                suggested_action: SuggestedAction::SaveForLater,
            },
        }
    }
}
