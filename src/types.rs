use serde::{Deserialize, Serialize};

/// A video fetched from a YouTube playlist
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub description: String,
    pub published_at: String,
    pub duration_seconds: Option<u64>,
}

/// Classification result from AI processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub category: VideoCategory,
    pub tags: Vec<String>,
    pub suggested_action: SuggestedAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VideoCategory {
    Tutorial,
    News,
    Concept,
    Entertainment,
    Tool,
    Health,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SuggestedAction {
    WatchFull,
    ReadTranscript,
    SaveForLater,
    Archive,
}

/// A processed video ready to create tasks/notes from
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedVideo {
    pub video: Video,
    pub transcript: Option<String>,
    pub summary: String,
    pub key_points: Vec<String>,
    pub classification: Classification,
}

/// State file: tracks which videos we've already processed
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessState {
    pub processed_ids: Vec<String>,
}

impl ProcessState {
    pub fn new() -> Self {
        Self {
            processed_ids: Vec::new(),
        }
    }

    pub fn is_processed(&self, id: &str) -> bool {
        self.processed_ids.contains(&id.to_string())
    }

    pub fn mark_processed(&mut self, id: String) {
        if !self.is_processed(&id) {
            self.processed_ids.push(id);
        }
    }
}

/// Task payload for Super Productivity API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpTask {
    pub title: String,
    pub notes: String,
    pub project_id: String,
    pub tag_ids: Vec<String>,
}

/// SP task from the API response
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SpTaskResponse {
    pub id: String,
    pub title: String,
}
