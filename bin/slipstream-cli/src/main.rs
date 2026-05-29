use anyhow::Context;
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use log::{debug, error, info, warn};
use slipstream_common::{Config, Metrics};
use slipstream_net::{
    blocklist::BlocklistManager, cartographer::Cartographer, engine::QuicEngine,
    geyser::spawn_geyser_monitor,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
#[allow(deprecated)]
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_log::LogTracer;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "slipstream")]
struct Cli {
    #[arg(short, long)]
    rpc: Option<String>,

    #[arg(long)]
    geyser: Option<String>,

    #[arg(short, long)]
    keypair: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Monitor,
    Fire {
        #[arg(short, long)]
        recipient: Option<String>,
        #[arg(long)]
        priority_fee: Option<u64>,
    },
    Spam {
        #[arg(short, long, default_value = "10")]
        count: u64,
        #[arg(short, long)]
        recipient: Option<String>,
        #[arg(long)]
        priority_fee: Option<u64>,
    },
    CompareLatency {
        #[arg(short, long, default_value = "20")]
        iterations: u64,
        #[arg(short, long)]
        recipient: Option<String>,
        #[arg(long)]
        priority_fee: Option<u64>,
        #[arg(long, default_value_t = false)]
        skip_rpc_preflight: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    init_tracing()?;

    let metrics = Arc::new(Metrics::default());
    let cli = Cli::parse();

    let mut config = Config::from_env().context("Invalid configuration")?;

    if let Some(rpc) = cli.rpc {
        config.rpc_url = rpc;
    }
    if let Some(geyser) = cli.geyser {
        config.geyser_url = Some(geyser);
    }

    let keypair_path = match cli.keypair {
        Some(p) => p,
        None => {
            let base = dirs::home_dir()
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| anyhow::anyhow!("Cannot determine home or current directory"))?;
            base.join(".config/solana/id.json")
        }
    };
    let identity = read_keypair_file(&keypair_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to load keypair from {:?}: {}. Use --keypair to specify path.",
            keypair_path,
            e
        )
    })?;
    info!("Identity: {}", identity.pubkey());

    info!("Initializing Shield (blocklist protection)...");
    let shield_manager = Arc::new(BlocklistManager::from_env());

    let loaded_count = shield_manager.load_local().await;
    if loaded_count > 0 {
        info!("Shield: Active with {} blocked validators", loaded_count);
    } else {
        warn!("Shield: No local blocklist found. Will fetch from remote.");
    }

    let _shield_updater = shield_manager.clone().spawn_updater();

    info!("Initializing Cartographer with RPC: {}", config.rpc_url);
    let cartographer = Arc::new(Cartographer::new(
        config.rpc_url.clone(),
        shield_manager.get_handle(),
    ));
    cartographer.refresh_topology().await?;
    cartographer.update_schedule().await?;

    if let Some(ref url) = config.geyser_url {
        info!("MODE: HYBRID (RPC Map + Geyser Clock)");
        info!("   Geyser Endpoint: {}", url);
        let startup_rx = spawn_geyser_monitor(
            url.clone(),
            cartographer.clone(),
            config.geyser_reconnect_delay(),
            config.geyser_max_reconnect_delay(),
            Arc::clone(&metrics),
        );

        match tokio::time::timeout(Duration::from_secs(10), startup_rx).await {
            Ok(Ok(Ok(()))) => {
                info!("Geyser: Initial connection established.");
            }
            Ok(Ok(Err(e))) => {
                warn!(
                    "Geyser: Initial connection failed: {}. Continuing with background retries.",
                    e
                );
                metrics.inc_geyser_reconnects();
            }
            Ok(Err(_)) => {
                warn!("Geyser: Startup signal lost. Continuing with background retries.");
                metrics.inc_geyser_reconnects();
            }
            Err(_) => {
                warn!(
                    "Geyser: Connection timed out after 10s. Continuing with background retries."
                );
                metrics.inc_geyser_reconnects();
            }
        }
    } else {
        info!("MODE: LEGACY (RPC Polling)");
        info!("   (Geyser URL not found in .env or args. Using fallback.)");
        let cart_clone = cartographer.clone();
        let poll_interval = config.rpc_poll_interval();
        tokio::spawn(async move {
            loop {
                if let Err(e) = cart_clone.fetch_rpc_slot().await {
                    debug!("RPC slot fetch failed: {}", e);
                }
                tokio::time::sleep(poll_interval).await;
            }
        });
    }

    info!("Initializing Engine...");
    let engine = Arc::new(QuicEngine::new(&identity, &config)?);

    let cart_clone = cartographer.clone();
    let engine_clone = engine.clone();
    let scout_interval = config.scout_interval();
    let lookahead = config.scout_lookahead_slots;
    tokio::spawn(async move {
        loop {
            let current_slot = cart_clone.get_known_slot();
            if current_slot > 0 {
                let upcoming = cart_clone
                    .get_upcoming_leaders(current_slot, lookahead)
                    .await;
                for target in upcoming {
                    debug!("Scout: Warming up connection to {}", target);
                    if let Err(e) = engine_clone.get_connection_handle(target).await {
                        debug!("Scout: Failed to warm connection to {}: {}", target, e);
                    }
                }
            }
            tokio::time::sleep(scout_interval).await;
        }
    });

    match cli.command {
        Commands::Monitor => monitor_loop(cartographer, config.monitor_interval(), metrics).await,
        Commands::Fire {
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(config.default_priority_fee);
            fire_transaction(
                &cartographer,
                &engine,
                &identity,
                to,
                fee,
                &config,
                &metrics,
            )
            .await?;
            log_metrics("fire", &metrics);
        }
        Commands::Spam {
            count,
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(config.default_priority_fee);
            spam_transactions(
                &cartographer,
                &engine,
                &identity,
                to,
                count,
                fee,
                &config,
                &metrics,
            )
            .await?;
            log_metrics("spam", &metrics);
        }
        Commands::CompareLatency {
            iterations,
            recipient,
            priority_fee,
            skip_rpc_preflight,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(config.default_priority_fee);
            compare_latency(
                &cartographer,
                &engine,
                &identity,
                to,
                iterations,
                fee,
                &config,
                &metrics,
                skip_rpc_preflight,
            )
            .await?;
        }
    }

    Ok(())
}

fn init_tracing() -> anyhow::Result<()> {
    let _ = LogTracer::init();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init()
        .ok();
    Ok(())
}

fn log_metrics(context: &str, metrics: &Metrics) {
    let s = metrics.snapshot();
    info!(
        "Metrics [{}]: attempted={} sent={} failed={} leader_lookup_failed={} geyser_reconnects={}",
        context,
        s.tx_attempted,
        s.tx_sent,
        s.tx_failed,
        s.leader_lookup_failed,
        s.geyser_reconnects
    );
}

fn parse_recipient(recipient: Option<String>, identity: &Keypair) -> anyhow::Result<Pubkey> {
    match recipient {
        Some(s) => s
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid recipient pubkey: '{}'. Expected base58.", s)),
        None => Ok(identity.pubkey()),
    }
}

async fn monitor_loop(
    cartographer: Arc<Cartographer>,
    interval: std::time::Duration,
    metrics: Arc<Metrics>,
) {
    info!("Starting Monitor Mode...");
    let mut tick: u64 = 0;
    loop {
        let slot = cartographer.get_known_slot();
        if slot > 0 {
            if let Some(target) = cartographer.get_target(slot).await {
                println!("Slot: {} | Leader IP: {}", slot, target);
            } else {
                println!("Slot: {} | Leader IP: UNKNOWN", slot);
            }
        }
        tick += 1;
        if tick % 10 == 0 {
            log_metrics("monitor", &metrics);
        }
        tokio::time::sleep(interval).await;
    }
}

async fn fire_transaction(
    cartographer: &Cartographer,
    engine: &QuicEngine,
    identity: &Keypair,
    recipient: Pubkey,
    priority_fee: u64,
    config: &Config,
    metrics: &Metrics,
) -> anyhow::Result<()> {
    let rpc = cartographer.rpc_client();
    let latest_blockhash = rpc.get_latest_blockhash().await?;

    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(config.default_compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee),
        system_instruction::transfer(&identity.pubkey(), &recipient, 1),
    ];

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&identity.pubkey()),
        &[identity],
        latest_blockhash,
    );
    let tx_bytes = bincode::serialize(&tx)?;

    metrics.inc_tx_attempted();
    let slot = cartographer.get_known_slot();
    if let Some(addr) = cartographer.get_target(slot).await {
        info!("Target: {}. Firing (Fee: {})...", addr, priority_fee);
        if let Err(e) = engine.send_transaction(addr, tx_bytes).await {
            metrics.inc_tx_failed();
            return Err(e.into());
        }
        metrics.inc_tx_sent();
        let sig = tx
            .signatures
            .first()
            .ok_or_else(|| anyhow::anyhow!("Transaction has no signatures"))?;
        info!("Sent! Sig: {}", sig);
    } else {
        metrics.inc_leader_lookup_failed();
        error!("No leader found for slot {}", slot);
    }
    Ok(())
}

async fn spam_transactions(
    cartographer: &Cartographer,
    engine: &QuicEngine,
    identity: &Keypair,
    recipient: Pubkey,
    count: u64,
    priority_fee: u64,
    config: &Config,
    metrics: &Metrics,
) -> anyhow::Result<()> {
    let rpc = cartographer.rpc_client();
    let latest_blockhash = rpc.get_latest_blockhash().await?;

    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(config.default_compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee),
        system_instruction::transfer(&identity.pubkey(), &recipient, 1),
    ];

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&identity.pubkey()),
        &[identity],
        latest_blockhash,
    );
    let tx_bytes = bincode::serialize(&tx)?;

    let slot = cartographer.get_known_slot();
    let target = cartographer
        .get_target(slot)
        .await
        .ok_or(anyhow::anyhow!("No leader found"))?;

    info!("Target Locked: {}", target);
    let connection = engine.get_connection_handle(target).await?;
    info!("Pipe Open. Firing {} rounds.", count);

    let mut success_count: u64 = 0;
    let mut fail_count: u64 = 0;
    for i in 0..count {
        metrics.inc_tx_attempted();
        match connection.open_uni().await {
            Ok(mut stream) => {
                if let Err(e) = stream.write_all(&tx_bytes).await {
                    warn!("Stream write failed (tx {}): {}", i, e);
                    fail_count += 1;
                    metrics.inc_tx_failed();
                    continue;
                }
                if let Err(e) = stream.finish() {
                    warn!("Stream finish failed (tx {}): {}", i, e);
                    fail_count += 1;
                    metrics.inc_tx_failed();
                    continue;
                }
                success_count += 1;
                metrics.inc_tx_sent();
            }
            Err(e) => {
                warn!("Failed to open stream (tx {}): {}", i, e);
                fail_count += 1;
                metrics.inc_tx_failed();
            }
        }
    }
    info!(
        "Firing Complete. Sent: {}, Failed: {}",
        success_count, fail_count
    );
    Ok(())
}

async fn compare_latency(
    cartographer: &Cartographer,
    engine: &QuicEngine,
    identity: &Keypair,
    recipient: Pubkey,
    iterations: u64,
    priority_fee: u64,
    config: &Config,
    metrics: &Metrics,
    skip_rpc_preflight: bool,
) -> anyhow::Result<()> {
    let rpc = cartographer.rpc_client();
    let sender = identity.pubkey();
    let sender_balance = rpc.get_balance(&sender).await?;
    if sender_balance == 0 {
        return Err(anyhow::anyhow!(
            "Sender wallet {} has 0 lamports on current cluster. Fund it first (e.g. devnet airdrop).",
            sender
        ));
    }

    let recipient_effective = match rpc.get_account(&recipient).await {
        Ok(_) => recipient,
        Err(_) => {
            warn!(
                "Recipient {} has no account on current cluster. Falling back to self-transfer for benchmark.",
                recipient
            );
            sender
        }
    };

    let mut direct_ms = Vec::new();
    let mut rpc_ms = Vec::new();

    println!(
        "Running latency comparison for {} iterations...",
        iterations
    );
    println!("Note: this sends real transactions on the configured cluster.");

    for i in 0..iterations {
        // Direct path
        let slot = cartographer.get_known_slot();
        let target = match cartographer.get_target(slot).await {
            Some(t) => t,
            None => {
                metrics.inc_leader_lookup_failed();
                warn!(
                    "[{}] direct path skipped: no leader for slot {}",
                    i + 1,
                    slot
                );
                continue;
            }
        };

        let blockhash_direct = rpc.get_latest_blockhash().await?;
        let tx_direct = build_signed_transfer_tx(
            identity,
            recipient_effective,
            priority_fee,
            config.default_compute_unit_limit,
            blockhash_direct,
        );
        let tx_direct_bytes = bincode::serialize(&tx_direct)?;

        metrics.inc_tx_attempted();
        let start_direct = Instant::now();
        match engine.send_transaction(target, tx_direct_bytes).await {
            Ok(()) => {
                metrics.inc_tx_sent();
                direct_ms.push(start_direct.elapsed().as_secs_f64() * 1000.0);
            }
            Err(e) => {
                metrics.inc_tx_failed();
                warn!("[{}] direct send failed: {}", i + 1, e);
            }
        }

        // RPC path
        let blockhash_rpc = rpc.get_latest_blockhash().await?;
        let tx_rpc = build_signed_transfer_tx(
            identity,
            recipient_effective,
            priority_fee,
            config.default_compute_unit_limit,
            blockhash_rpc,
        );

        metrics.inc_tx_attempted();
        let start_rpc = Instant::now();
        match rpc
            .send_transaction_with_config(
                &tx_rpc,
                RpcSendTransactionConfig {
                    skip_preflight: skip_rpc_preflight,
                    ..RpcSendTransactionConfig::default()
                },
            )
            .await
        {
            Ok(_) => {
                metrics.inc_tx_sent();
                rpc_ms.push(start_rpc.elapsed().as_secs_f64() * 1000.0);
            }
            Err(e) => {
                metrics.inc_tx_failed();
                warn!("[{}] rpc send failed: {}", i + 1, e);
            }
        }
    }

    print_latency_summary("Direct QUIC", &direct_ms);
    print_latency_summary("RPC Submit", &rpc_ms);
    log_metrics("compare-latency", metrics);

    Ok(())
}

fn build_signed_transfer_tx(
    identity: &Keypair,
    recipient: Pubkey,
    priority_fee: u64,
    compute_limit: u32,
    blockhash: solana_sdk::hash::Hash,
) -> Transaction {
    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(compute_limit),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee),
        system_instruction::transfer(&identity.pubkey(), &recipient, 1),
    ];

    Transaction::new_signed_with_payer(
        &instructions,
        Some(&identity.pubkey()),
        &[identity],
        blockhash,
    )
}

fn print_latency_summary(label: &str, samples_ms: &[f64]) {
    if samples_ms.is_empty() {
        println!("{}: no successful samples", label);
        return;
    }

    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let p50 = percentile(&sorted, 0.50);
    let p95 = percentile(&sorted, 0.95);
    let p99 = percentile(&sorted, 0.99);
    let avg = sorted.iter().sum::<f64>() / sorted.len() as f64;

    println!(
        "{} -> n={} avg={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms",
        label,
        sorted.len(),
        avg,
        p50,
        p95,
        p99
    );
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[rank]
}
