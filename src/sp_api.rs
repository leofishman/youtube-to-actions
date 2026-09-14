use crate::types::SpTask;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;

/// Where Super Productivity persists the local REST API token (0600, minted when
/// the API is enabled). Flatpak first, then a native/AppImage install.
fn token_paths() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    vec![
        home.join(".var/app/com.super_productivity.SuperProductivity/config/superProductivity/local-rest-api-token"),
        home.join(".config/superProductivity/local-rest-api-token"),
    ]
}

/// First readable, non-empty file of `paths`, trimmed.
fn read_first_existing(paths: &[PathBuf]) -> Option<String> {
    paths.iter().find_map(|p| {
        let t = std::fs::read_to_string(p).ok()?;
        let t = t.trim().to_string();
        (!t.is_empty()).then_some(t)
    })
}

/// SP v19 made the local API token-authenticated: every route except /health
/// answers 401 without `Authorization: Bearer <token>`. The token is not in the
/// app state (it belongs to the Electron main process), so it is read from its
/// file. `SP_API_TOKEN` overrides, for non-standard installs.
fn find_token() -> Option<String> {
    match std::env::var("SP_API_TOKEN") {
        Ok(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        _ => read_first_existing(&token_paths()),
    }
}

/// Super Productivity Local REST API client
pub struct SpClient {
    client: reqwest::Client,
    base_url: String,
    has_token: bool,
}

impl SpClient {
    pub fn new() -> Self {
        let token = find_token();
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(ref t) = token
            && let Ok(mut v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {t}"))
        {
            v.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, v);
        }
        Self {
            client: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            base_url: "http://127.0.0.1:3876".to_string(),
            has_token: token.is_some(),
        }
    }

    /// Message for the 401 that a missing or stale token produces.
    fn auth_hint(&self) -> String {
        if self.has_token {
            format!(
                "el token no fue aceptado — si lo regeneraste en SP, se relee solo de {}",
                token_paths()[0].display()
            )
        } else {
            format!(
                "no encontré el token de la API de SP (busqué en {} y en SP_API_TOKEN). \
                 Habilitá la API en Settings → Misc y volvé a intentar",
                token_paths()[0].display()
            )
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
            let msg = resp["error"]["message"].as_str().unwrap_or("unknown");
            if resp["error"]["code"].as_str() == Some("UNAUTHORIZED") {
                anyhow::bail!("SP API 401: {}", self.auth_hint());
            }
            anyhow::bail!("SP API error: {}", msg);
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
            let msg = resp["error"]["message"].as_str().unwrap_or("unknown");
            if resp["error"]["code"].as_str() == Some("UNAUTHORIZED") {
                anyhow::bail!("SP API 401 listando proyectos: {}", self.auth_hint());
            }
            anyhow::bail!("SP API error listing projects: {}", msg);
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_first_existing_skips_missing_and_empty() {
        let dir = std::env::temp_dir().join(format!("yt2action-sp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("no-existe");
        let empty = dir.join("vacio");
        let good = dir.join("token");
        std::fs::write(&empty, "  \n").unwrap();
        std::fs::write(&good, "  abc123\n").unwrap();

        assert_eq!(read_first_existing(&[missing.clone()]), None);
        // el vacío no gana: se salta y sigue buscando, y el token llega sin espacios
        assert_eq!(
            read_first_existing(&[missing, empty, good]),
            Some("abc123".to_string())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn token_paths_cover_flatpak_and_native() {
        let paths = token_paths();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].to_string_lossy().contains(".var/app/com.super_productivity"));
        assert!(paths[1].to_string_lossy().ends_with(".config/superProductivity/local-rest-api-token"));
    }
}
