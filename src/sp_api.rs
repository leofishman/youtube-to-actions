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

    /// List projects in Super Productivity
    pub async fn list_projects(&self) -> Result<Vec<Value>> {
        let resp = self
            .client
            .get(format!("{}/projects", self.base_url))
            .send()
            .await
            .context("SP API list projects request failed")?
            .json::<Value>()
            .await
            .context("Failed to parse SP projects response")?;

        if resp["ok"].as_bool().unwrap_or(false) {
            let data = resp["data"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            Ok(data)
        } else {
            anyhow::bail!(
                "SP API error listing projects: {}",
                resp["error"]["message"].as_str().unwrap_or("unknown")
            );
        }
    }

    /// Resolve project ID by title (case-insensitive)
    pub async fn resolve_project_id(&self, title: &str) -> Result<Option<String>> {
        let projects = match self.list_projects().await {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to fetch SP projects for resolution: {}", e);
                return Ok(None);
            }
        };
        let search_title = title.to_lowercase();
        
        // 1. Try case-insensitive title match
        for p in &projects {
            if let Some(t) = p["title"].as_str() {
                if t.to_lowercase() == search_title {
                    if let Some(id) = p["id"].as_str() {
                        return Ok(Some(id.to_string()));
                    }
                }
            }
        }
        
        // 2. Try direct ID match
        for p in &projects {
            if let Some(id) = p["id"].as_str() {
                if id == title {
                    return Ok(Some(id.to_string()));
                }
            }
        }

        Ok(None)
    }
}
