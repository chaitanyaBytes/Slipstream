use log::{debug, info, warn};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

pub type BlocklistHandle = Arc<RwLock<HashSet<Pubkey>>>;

pub struct BlocklistManager {
    blocklist: BlocklistHandle,
    local_path: PathBuf,
    remote_url: Option<String>,
    refresh_interval: Duration,
}

impl BlocklistManager {
    pub fn new() -> Self {
        Self::with_config(
            PathBuf::from("./blocklist.txt"),
            None,
            DEFAULT_REFRESH_INTERVAL,
        )
    }

    pub fn with_config(
        local_path: PathBuf,
        remote_url: Option<String>,
        refresh_interval: Duration,
    ) -> Self {
        Self {
            blocklist: Arc::new(RwLock::new(HashSet::new())),
            local_path,
            remote_url,
            refresh_interval,
        }
    }

    pub fn from_env() -> Self {
        let local_path = std::env::var("SLIPSTREAM_BLOCKLIST_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./blocklist.txt"));

        let remote_url = std::env::var("SLIPSTREAM_BLOCKLIST_URL").ok();

        let refresh_interval = std::env::var("SLIPSTREAM_BLOCKLIST_REFRESH_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_REFRESH_INTERVAL);

        Self::with_config(local_path, remote_url, refresh_interval)
    }

    pub fn get_handle(&self) -> BlocklistHandle {
        self.blocklist.clone()
    }

    pub async fn load_local(&self) -> usize {
        match self.load_from_file(&self.local_path).await {
            Ok(keys) => {
                let count = keys.len();
                if count > 0 {
                    let mut guard = self.blocklist.write().await;
                    *guard = keys;
                    info!(
                        "shield: loaded {} blocked validators from {:?}",
                        count, self.local_path
                    );
                } else {
                    info!("shield: blocklist {:?} is empty", self.local_path);
                }
                count
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!("shield: no blocklist file at {:?}", self.local_path);
                0
            }
            Err(e) => {
                warn!(
                    "shield: failed to load blocklist {:?}: {}",
                    self.local_path, e
                );
                0
            }
        }
    }

    pub async fn fetch_remote(&self) -> Result<usize, String> {
        let url = self
            .remote_url
            .as_ref()
            .ok_or_else(|| "no remote url configured (local-only mode)".to_string())?;

        debug!("shield: fetching blocklist from {}", url);

        let response = reqwest::get(url)
            .await
            .map_err(|e| format!("http request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("http error: {}", response.status()));
        }

        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read response body: {}", e))?;

        let keys = self.parse_blocklist(&body);
        if keys.is_empty() {
            return Err("remote blocklist is empty. ignoring update".into());
        }

        let count = keys.len();

        if let Err(e) = self.persist_to_file(&keys).await {
            warn!("shield: failed to persist blocklist: {}", e);
        }

        {
            let mut guard = self.blocklist.write().await;
            *guard = keys;
        }

        info!("shield: updated blocklist with {} validators", count);
        Ok(count)
    }

    pub async fn reload_local(&self) -> usize {
        self.load_local().await
    }

    pub fn spawn_updater(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            if manager.remote_url.is_some() {
                if let Err(e) = manager.fetch_remote().await {
                    warn!("shield: initial remote fetch failed: {}", e);
                }
            }

            loop {
                tokio::time::sleep(manager.refresh_interval).await;

                if manager.remote_url.is_some() {
                    if let Err(e) = manager.fetch_remote().await {
                        debug!("shield: remote fetch failed, reloading local: {}", e);
                        manager.reload_local().await;
                    }
                } else {
                    manager.reload_local().await;
                }
            }
        })
    }

    fn parse_blocklist(&self, content: &str) -> HashSet<Pubkey> {
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return None;
                }
                match Pubkey::from_str(trimmed) {
                    Ok(pk) => Some(pk),
                    Err(_) => {
                        debug!("shield: skipping invalid pubkey: {}", trimmed);
                        None
                    }
                }
            })
            .collect()
    }

    async fn load_from_file(&self, path: &Path) -> Result<HashSet<Pubkey>, std::io::Error> {
        let content = tokio::fs::read_to_string(path).await?;
        Ok(self.parse_blocklist(&content))
    }

    async fn persist_to_file(&self, keys: &HashSet<Pubkey>) -> Result<(), std::io::Error> {
        let content: String = keys.iter().map(|pk| format!("{}\n", pk)).collect();
        tokio::fs::write(&self.local_path, content).await?;
        Ok(())
    }
}

impl Default for BlocklistManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_blocklist_skips_invalid_and_comments() {
        let manager = BlocklistManager::new();
        let content = r#"
            # comment
            11111111111111111111111111111112
            invalid_key
            So11111111111111111111111111111111111111112
        "#;

        let keys = manager.parse_blocklist(content);
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn parse_empty_blocklist() {
        let manager = BlocklistManager::new();
        let keys = manager.parse_blocklist("");
        assert!(keys.is_empty());
    }

    #[test]
    fn from_env_defaults() {
        std::env::remove_var("SLIPSTREAM_BLOCKLIST_FILE");
        std::env::remove_var("SLIPSTREAM_BLOCKLIST_URL");
        std::env::remove_var("SLIPSTREAM_BLOCKLIST_REFRESH_SECS");

        let manager = BlocklistManager::from_env();
        assert_eq!(manager.local_path, PathBuf::from("./blocklist.txt"));
        assert!(manager.remote_url.is_none());
        assert_eq!(manager.refresh_interval, DEFAULT_REFRESH_INTERVAL);
    }
}
