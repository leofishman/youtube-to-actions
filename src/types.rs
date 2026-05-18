use serde::{Deserialize, Serialize};

/// A video fetched from a YouTube playlist
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    /// The playlist item ID (used to remove videos from a playlist)
    pub playlist_item_id: String,
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
/// Suggested action from AI analysis
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum SuggestedAction {
    WatchFull,
    ReadTranscript,
    SaveForLater,
    Archive,
}

impl std::fmt::Display for SuggestedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SuggestedAction::WatchFull => write!(f, "Ver completo"),
            SuggestedAction::ReadTranscript => write!(f, "Leer resumen"),
            SuggestedAction::SaveForLater => write!(f, "Guardar para después"),
            SuggestedAction::Archive => write!(f, "Archivar"),
        }
    }
}

/// A processed video ready to create tasks/notes from
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedVideo {
    pub video: Video,
    pub transcript: Option<String>,
    pub summary: String,
    pub key_points: Vec<String>,
    pub classification: Classification,
    /// Vault folder suggested by the AI based on video content
    pub target_folder: String,
    /// Path to the local video file (e.g. ~/Videos/yt2action/<id>/...)
    pub local_video_path: Option<String>,
    /// Optional rich analysis from a Fabric pattern
    pub fabric_output: Option<String>,
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

/// Result of processing a single video (for the final report)
#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub video_title: String,
    pub video_url: String,
    pub target_folder: String,
    pub note_path: Option<String>,
    pub sp_task_id: Option<String>,
    pub moved_to_processed: bool,
    pub suggested_action: SuggestedAction,
    pub tags: Vec<String>,
    pub error: Option<String>,
}
