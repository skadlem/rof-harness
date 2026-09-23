//! Provider credentials: ~/.rof/credentials (0600), ROF_CREDENTIALS override
//! for tests. Env keys take precedence at call time (main.rs), so this store
//! is the interactive path to the same secrets, never the only path.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Store {
    path: PathBuf,
}

/// Resolve the store path. A test override wins so tests never touch home.
pub fn store() -> Store {
    let path = std::env::var("ROF_CREDENTIALS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".rof/credentials"))
                .unwrap_or_else(|_| PathBuf::from(".rof-credentials"))
        });
    Store { path }
}

fn read_all(path: &PathBuf) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Set `var` to `value` only when it is unset or blank (env wins).
fn fill(var: &str, value: &str) {
    let blank = std::env::var(var)
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if blank {
        std::env::set_var(var, value);
    }
}

impl Store {
    /// Plugged-in provider names only — never keys.
    pub fn providers(&self) -> Vec<String> {
        read_all(&self.path).keys().cloned().collect()
    }

    /// Insert or overwrite a key. Creates parents, enforces 0600.
    pub fn save(&self, provider: &str, key: &str) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut map = read_all(&self.path);
        map.insert(provider.to_string(), key.to_string());
        std::fs::write(&self.path, serde_json::to_string_pretty(&map)?)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &self.path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        Ok(())
    }

    /// Remove a provider. Returns false when it was not present.
    pub fn remove(&self, provider: &str) -> anyhow::Result<bool> {
        let mut map = read_all(&self.path);
        let hit = map.remove(provider).is_some();
        if hit {
            std::fs::write(&self.path, serde_json::to_string_pretty(&map)?)?;
        }
        Ok(hit)
    }

    /// Copy stored keys into the process env where the matching env knob
    /// is empty, so a `/login` survives into the next goal's rebuilt
    /// clients. Env always wins per var (spec §6: env keys take precedence
    /// and skip login) — this only fills blanks, never overwrites.
    pub fn export_missing_env(&self) {
        let map = read_all(&self.path);
        for (provider, key) in &map {
            if key.trim().is_empty() {
                continue;
            }
            match provider.as_str() {
                "openrouter" => fill("OR_TOKEN", key),
                "go" => {
                    fill("ROF_TOKEN", key);
                    fill("ROF_CHAT_BASE", "https://opencode.ai/zen/go/v1");
                }
                "atria" => {
                    fill("ROF_TOKEN", key);
                    fill("ROF_CHAT_BASE", "https://api.atria-asi.ai/v1");
                }
                "custom" => fill("ROF_TOKEN", key),
                _ => {}
            }
        }
    }
}

/// Model-list base per known provider. `custom` resolves its base from
/// `ROF_CHAT_BASE` (it has no fixed home). Unknown names error here,
/// before any network, so the offline test stays offline.
fn base_for(provider: &str) -> Result<String, String> {
    match provider {
        "openrouter" => Ok("https://openrouter.ai/api/v1".to_string()),
        "go" => Ok("https://opencode.ai/zen/go/v1".to_string()),
        "atria" => Ok("https://api.atria-asi.ai/v1".to_string()),
        "custom" => std::env::var("ROF_CHAT_BASE")
            .ok()
            .filter(|b| !b.trim().is_empty())
            .map(|b| b.trim().trim_end_matches('/').to_string())
            .ok_or_else(|| "custom provider needs ROF_CHAT_BASE set to its base URL".to_string()),
        _ => Err(format!(
            "unknown provider '{provider}' (openrouter/go/atria/custom)"
        )),
    }
}

static STATUS: std::sync::Mutex<BTreeMap<String, String>> = std::sync::Mutex::new(BTreeMap::new());

/// Remember a verify outcome for `/models` (cached, never re-fetched).
/// `run.rs` records failures too, so the listing reflects the session.
pub fn remember_status(provider: &str, status: &str) {
    if let Ok(mut m) = STATUS.lock() {
        m.insert(provider.to_string(), status.to_string());
    }
}

/// Cached verify outcomes (`ok` / `auth-failed` / ...), empty until the
/// session verifies something.
pub fn statuses() -> BTreeMap<String, String> {
    STATUS.lock().map(|m| m.clone()).unwrap_or_default()
}

/// Verify a key with one cheap model-list call; return model ids.
/// Unknown providers error before touching the network so the test stays
/// offline. The key rides an `Authorization: Bearer` header; a 401/403
/// is an explicit auth failure (not "empty list") so `/login` can say so.
pub async fn verify(provider: &str, key: &str) -> Result<Vec<String>, String> {
    let base = base_for(provider)?;
    let url = format!("{base}/models");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {key}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        remember_status(provider, "auth-failed");
        return Err(format!(
            "{provider}: key rejected ({status}); check the key and retry /login"
        ));
    }
    if !status.is_success() {
        return Err(format!("{provider}: verify call failed ({status})"));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let ids = v
        .pointer("/data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    m.pointer("/id")
                        .and_then(|i| i.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    remember_status(provider, "ok");
    Ok(ids)
}
