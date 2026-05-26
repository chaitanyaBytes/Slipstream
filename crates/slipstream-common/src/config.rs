use crate::error::SlipstreamError;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub geyser_url: Option<String>,
    pub rpc_poll_interval_ms: u64,
    pub scout_interval_ms: u64,
    pub scout_lookahead_slots: u64,
    pub monitor_interval_ms: u64,
    pub default_compute_unit_limit: u32,
    pub default_priority_fee: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, SlipstreamError> {
        let cfg = Self {
            rpc_url: env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            geyser_url: env::var("GEYSER_URL").ok(),
            rpc_poll_interval_ms: parse_env("RPC_POLL_INTERVAL_MS", 400),
            scout_interval_ms: parse_env("SCOUT_INTERVAL_MS", 1000),
            scout_lookahead_slots: parse_env("SCOUT_LOOKAHEAD_SLOTS", 10),
            monitor_interval_ms: parse_env("MONITOR_INTERVAL_MS", 400),
            default_compute_unit_limit: parse_env("DEFAULT_COMPUTE_UNIT_LIMIT", 200_000),
            default_priority_fee: parse_env("DEFAULT_PRIORITY_FEE", 100_000),
        };

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), SlipstreamError> {
        const MIN_INTERVAL_MS: u64 = 50;

        if self.rpc_poll_interval_ms < MIN_INTERVAL_MS {
            return Err(SlipstreamError::ConfigValidation(format!(
                "RPC_POLL_INTERVAL_MS must be >= {MIN_INTERVAL_MS}"
            )));
        }

        if self.scout_interval_ms < MIN_INTERVAL_MS {
            return Err(SlipstreamError::ConfigValidation(format!(
                "SCOUT_INTERVAL_MS must be >= {MIN_INTERVAL_MS}"
            )));
        }

        if self.monitor_interval_ms < MIN_INTERVAL_MS {
            return Err(SlipstreamError::ConfigValidation(format!(
                "MONITOR_INTERVAL_MS must be >= {MIN_INTERVAL_MS}"
            )));
        }

        if self.default_compute_unit_limit == 0 {
            return Err(SlipstreamError::ConfigValidation(
                "DEFAULT_COMPUTE_UNIT_LIMIT must be > 0".to_string(),
            ));
        }

        Ok(())
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse::<T>().ok())
        .unwrap_or(default)
}
