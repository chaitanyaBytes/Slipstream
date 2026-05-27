use log::info;
use slipstream_common::SlipstreamError;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Cartographer {
    rpc: Arc<RpcClient>,
    node_map: Arc<RwLock<HashMap<Pubkey, SocketAddr>>>,
    schedule: Arc<RwLock<HashMap<u64, Pubkey>>>,
    current_slot: Arc<AtomicU64>,
    current_epoch: Arc<AtomicU64>,
}

impl Cartographer {
    pub fn new(rpc_url: String) -> Self {
        Self {
            rpc: Arc::new(RpcClient::new(rpc_url)),
            node_map: Arc::new(RwLock::new(HashMap::new())),
            schedule: Arc::new(RwLock::new(HashMap::new())),
            current_slot: Arc::new(AtomicU64::new(0)),
            current_epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get_known_slot(&self) -> u64 {
        self.current_slot.load(Ordering::Relaxed)
    }

    pub fn update_slot(&self, slot: u64) {
        self.current_slot.store(slot, Ordering::Relaxed);
    }

    pub async fn get_target(&self, slot: u64) -> Option<SocketAddr> {
        let leader = {
            let schedule = self.schedule.read().await;
            schedule.get(&slot).cloned()?
        };

        let map = self.node_map.read().await;
        map.get(&leader).copied()
    }

    pub async fn get_upcoming_leaders(&self, current_slot: u64, lookahead: u64) -> Vec<SocketAddr> {
        let schedule = self.schedule.read().await;
        let node_map = self.node_map.read().await;

        let mut out = Vec::new();
        for i in 1..=lookahead {
            let slot = current_slot + i;
            if let Some(pubkey) = schedule.get(&slot) {
                if let Some(addr) = node_map.get(pubkey) {
                    if !out.contains(addr) {
                        out.push(*addr);
                    }
                }
            }
        }

        out
    }

    pub async fn refresh_topology(&self) -> Result<(), SlipstreamError> {
        info!("refreshing cluster topology...");
        let nodes = self.rpc.get_cluster_nodes().await.map_err(|e| {
            SlipstreamError::ConfigValidation(format!("rpc get_cluster_nodes failed: {e}"))
        })?;

        let mut next = HashMap::new();
        for node in nodes {
            if let (Some(tpu_quic), Ok(pubkey)) = (node.tpu_quic, Pubkey::from_str(&node.pubkey)) {
                next.insert(pubkey, tpu_quic);
            }
        }

        let mut guard = self.node_map.write().await;
        *guard = next;
        info!("topology loaded: {} quic validators", guard.len());
        Ok(())
    }

    pub async fn update_schedule(&self) -> Result<(), SlipstreamError> {
        let epoch_info = self.rpc.get_epoch_info().await.map_err(|e| {
            SlipstreamError::ConfigValidation(format!("rpc get_epoch_info failed: {e}"))
        })?;

        let known_epoch = self.current_epoch.load(Ordering::Relaxed);
        if known_epoch == epoch_info.epoch && known_epoch != 0 {
            self.update_slot(epoch_info.absolute_slot);
            return Ok(());
        }

        info!(
            "new epoch {} detected, refreshing leader schedule",
            epoch_info.epoch
        );
        let schedule_data = self
            .rpc
            .get_leader_schedule(None)
            .await
            .map_err(|e| {
                SlipstreamError::ConfigValidation(format!("rpc get_leader_schedule failed: {e}"))
            })?
            .ok_or_else(|| {
                SlipstreamError::ConfigValidation("leader schedule unavailable".to_string())
            })?;

        let epoch_start_slot = epoch_info.absolute_slot - epoch_info.slot_index;
        let mut next = HashMap::new();
        for (pubkey_str, relative_slots) in schedule_data {
            if let Ok(pubkey) = Pubkey::from_str(&pubkey_str) {
                for rel_slot in relative_slots {
                    next.insert(epoch_start_slot + rel_slot as u64, pubkey);
                }
            }
        }

        let mut guard = self.schedule.write().await;
        *guard = next;
        self.current_epoch
            .store(epoch_info.epoch, Ordering::Relaxed);
        self.update_slot(epoch_info.absolute_slot);

        Ok(())
    }

    pub async fn fetch_rpc_slot(&self) -> Result<u64, SlipstreamError> {
        let slot =
            self.rpc.get_slot().await.map_err(|e| {
                SlipstreamError::ConfigValidation(format!("rpc get_slot failed: {e}"))
            })?;
        self.update_slot(slot);
        Ok(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::Cartographer;

    #[test]
    fn slot_clock_basics() {
        let cartographer = Cartographer::new("http://localhost:8899".to_string());
        assert_eq!(cartographer.get_known_slot(), 0);
        cartographer.update_slot(42);
        assert_eq!(cartographer.get_known_slot(), 42);
    }
}
