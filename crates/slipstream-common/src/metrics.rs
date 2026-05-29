use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    tx_attempted: AtomicU64,
    tx_sent: AtomicU64,
    tx_failed: AtomicU64,
    leader_lookup_failed: AtomicU64,
    geyser_reconnects: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
pub struct MetricsSnapshot {
    pub tx_attempted: u64,
    pub tx_sent: u64,
    pub tx_failed: u64,
    pub leader_lookup_failed: u64,
    pub geyser_reconnects: u64,
}

impl Metrics {
    pub fn inc_tx_attempted(&self) {
        self.tx_attempted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_tx_sent(&self) {
        self.tx_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_tx_failed(&self) {
        self.tx_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_leader_lookup_failed(&self) {
        self.leader_lookup_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_geyser_reconnects(&self) {
        self.geyser_reconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            tx_attempted: self.tx_attempted.load(Ordering::Relaxed),
            tx_sent: self.tx_sent.load(Ordering::Relaxed),
            tx_failed: self.tx_failed.load(Ordering::Relaxed),
            leader_lookup_failed: self.leader_lookup_failed.load(Ordering::Relaxed),
            geyser_reconnects: self.geyser_reconnects.load(Ordering::Relaxed),
        }
    }
}
