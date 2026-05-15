use crate::types::SpTask;
use anyhow::{Context, Result};
use serde_json::Value;

/// Super Productivity Local REST API client
pub struct SpClient {
    client: reqwest::Client,
    base_url: String,
}

impl SpClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: "http://127.0.0.1:3876".to_string(),
        }
    }

    /// Create a task in Super Productivity
    pub async fn create_task(&self, task: &SpTask) -> Result<String> {
        let body = serde_json::json!({
            "title": task.title,
            "notes": task.notes,
            "projectId": task.project_id,
            "tagIds": task.tag_ids,
        });

        let resp = self
            .client
            .post(format!("{}/tasks", self.base_url))
            .json(&body)
            .send()
            .await
            .context("SP API request failed")?
            .json::<Value>()
            .await
            .context("Failed to parse SP response")?;

        if resp["ok"].as_bool().unwrap_or(false) {
            Ok(resp["data"]["id"]
                .as_str()
                .unwrap_or("unknown")
                .to_string())
        } else {
            anyhow::bail!(
                "SP API error: {}",
                resp["error"]["message"].as_str().unwrap_or("unknown")
            );
        }
    }

    /// Health check
    pub async fn health_check(&self) -> Result<bool> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .context("SP health check failed")?
            .json::<Value>()
            .await?;

        Ok(resp["ok"].as_bool().unwrap_or(false))
    }
}
