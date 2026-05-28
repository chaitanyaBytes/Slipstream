use anyhow::Context;
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use slipstream_common::Config;
use slipstream_net::{spawn_geyser_monitor, BlocklistManager, Cartographer, QuicEngine};
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
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(name = "slipstream")]
#[command(about = "Low-latency Solana transaction sender")]
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

#[derive(Debug, Subcommand)]
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();

    let cli = Cli::parse();
    let mut cfg = Config::from_env().context("failed to load config")?;

    if let Some(rpc) = cli.rpc {
        cfg.rpc_url = rpc;
    }
    if let Some(geyser) = cli.geyser {
        cfg.geyser_url = Some(geyser);
    }

    let keypair_path = cli.keypair.unwrap_or_else(default_keypair_path);
    let identity = Arc::new(read_keypair_file(&keypair_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to load keypair from {:?}: {}. use --keypair to specify path",
            keypair_path,
            e
        )
    })?);

    let shield_manager = Arc::new(BlocklistManager::from_env());
    let loaded_count = shield_manager.load_local().await;
    if loaded_count > 0 {
        println!("shield active with {} blocked validators", loaded_count);
    } else {
        println!("shield local blocklist empty or missing");
    }
    let _shield_updater = shield_manager.clone().spawn_updater();

    match cli.command {
        Commands::Monitor => {
            monitor_loop(
                &cfg,
                &keypair_path.display().to_string(),
                Arc::clone(&identity),
                shield_manager.get_handle(),
            )
            .await?
        }
        Commands::Fire {
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(cfg.default_priority_fee);
            fire_transaction(&cfg, &identity, to, fee, shield_manager.get_handle()).await?;
        }
        Commands::Spam {
            count,
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(cfg.default_priority_fee);
            spam_transactions(&cfg, &identity, to, count, fee, shield_manager.get_handle()).await?;
        }
    }

    Ok(())
}

async fn monitor_loop(
    cfg: &Config,
    keypair_path: &str,
    identity: Arc<Keypair>,
    blocklist: slipstream_net::blocklist::BlocklistHandle,
) -> anyhow::Result<()> {
    println!("[monitor] starting");
    println!("rpc: {}", cfg.rpc_url);
    println!("geyser: {}", cfg.geyser_url.as_deref().unwrap_or("<none>"));
    println!("keypair: {keypair_path}");

    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone(), blocklist));
    cartographer
        .refresh_topology()
        .await
        .context("failed to refresh topology")?;
    cartographer
        .update_schedule()
        .await
        .context("failed to load leader schedule")?;

    if let Some(url) = cfg.geyser_url.clone() {
        let startup_rx = spawn_geyser_monitor(
            url,
            Arc::clone(&cartographer),
            cfg.geyser_reconnect_delay(),
            cfg.geyser_max_reconnect_delay(),
        );

        match tokio::time::timeout(Duration::from_secs(10), startup_rx).await {
            Ok(Ok(Ok(()))) => println!("mode: hybrid (rpc map + geyser clock)"),
            Ok(Ok(Err(e))) => {
                println!("geyser startup failed: {}. using rpc polling fallback", e);
                spawn_rpc_slot_poller(Arc::clone(&cartographer), cfg.rpc_poll_interval());
            }
            Ok(Err(_)) => {
                println!("geyser startup signal lost. using rpc polling fallback");
                spawn_rpc_slot_poller(Arc::clone(&cartographer), cfg.rpc_poll_interval());
            }
            Err(_) => {
                println!("geyser startup timed out. using rpc polling fallback");
                spawn_rpc_slot_poller(Arc::clone(&cartographer), cfg.rpc_poll_interval());
            }
        }
    } else {
        println!("mode: legacy (rpc polling)");
        spawn_rpc_slot_poller(Arc::clone(&cartographer), cfg.rpc_poll_interval());
    }

    let scout_interval = cfg.scout_interval();
    let scout_lookahead_slots = cfg.scout_lookahead_slots;

    {
        let c = Arc::clone(&cartographer);
        let engine = Arc::new(QuicEngine::new(&identity)?);
        tokio::spawn(async move {
            loop {
                let slot = c.get_known_slot();
                if slot > 0 {
                    let upcoming = c.get_upcoming_leaders(slot, scout_lookahead_slots).await;
                    for target in upcoming {
                        let _ = engine.get_connection_handle(target).await;
                    }
                }
                tokio::time::sleep(scout_interval).await;
            }
        });
    }

    loop {
        let slot = cartographer.get_known_slot();
        if slot == 0 {
            println!("slot: loading...");
        } else if let Some(target) = cartographer.get_target(slot).await {
            println!("slot: {slot} | leader: {target}");
        } else {
            println!("slot: {slot} | leader: <unknown>");
        }

        tokio::time::sleep(cfg.monitor_interval()).await;
    }
}

fn spawn_rpc_slot_poller(cartographer: Arc<Cartographer>, poll_interval: Duration) {
    tokio::spawn(async move {
        loop {
            let _ = cartographer.fetch_rpc_slot().await;
            tokio::time::sleep(poll_interval).await;
        }
    });
}

async fn fire_transaction(
    cfg: &Config,
    identity: &Arc<Keypair>,
    recipient: Pubkey,
    priority_fee: u64,
    blocklist: slipstream_net::blocklist::BlocklistHandle,
) -> anyhow::Result<()> {
    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone(), blocklist));
    cartographer
        .refresh_topology()
        .await
        .context("failed to refresh topology")?;
    cartographer
        .update_schedule()
        .await
        .context("failed to load leader schedule")?;

    let slot = cartographer.get_known_slot();
    let target = cartographer
        .get_target(slot)
        .await
        .ok_or_else(|| anyhow::anyhow!("no leader found for slot {}", slot))?;

    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(cfg.rpc_url.clone());
    let latest_blockhash = rpc.get_latest_blockhash().await?;

    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(cfg.default_compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee),
        system_instruction::transfer(&identity.pubkey(), &recipient, 1),
    ];

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&identity.pubkey()),
        &[identity.as_ref()],
        latest_blockhash,
    );

    let tx_bytes = bincode::serialize(&tx)?;

    let engine = QuicEngine::new(identity)?;
    engine.send_transaction(target, tx_bytes).await?;

    let sig = tx
        .signatures
        .first()
        .ok_or_else(|| anyhow::anyhow!("transaction has no signatures"))?;
    println!("sent tx to {target} | signature: {sig}");
    Ok(())
}

async fn spam_transactions(
    cfg: &Config,
    identity: &Arc<Keypair>,
    recipient: Pubkey,
    count: u64,
    priority_fee: u64,
    blocklist: slipstream_net::blocklist::BlocklistHandle,
) -> anyhow::Result<()> {
    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone(), blocklist));
    cartographer
        .refresh_topology()
        .await
        .context("failed to refresh topology")?;
    cartographer
        .update_schedule()
        .await
        .context("failed to load leader schedule")?;

    let slot = cartographer.get_known_slot();
    let target = cartographer
        .get_target(slot)
        .await
        .ok_or_else(|| anyhow::anyhow!("no leader found for slot {}", slot))?;

    let rpc = solana_client::nonblocking::rpc_client::RpcClient::new(cfg.rpc_url.clone());
    let latest_blockhash = rpc.get_latest_blockhash().await?;

    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(cfg.default_compute_unit_limit),
        ComputeBudgetInstruction::set_compute_unit_price(priority_fee),
        system_instruction::transfer(&identity.pubkey(), &recipient, 1),
    ];

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&identity.pubkey()),
        &[identity.as_ref()],
        latest_blockhash,
    );
    let tx_bytes = bincode::serialize(&tx)?;

    let engine = QuicEngine::new(identity)?;
    let connection = engine.get_connection_handle(target).await?;

    let mut sent = 0_u64;
    let mut failed = 0_u64;

    for i in 0..count {
        match connection.open_uni().await {
            Ok(mut stream) => {
                if let Err(e) = stream.write_all(&tx_bytes).await {
                    eprintln!("stream write failed (tx {}): {}", i, e);
                    failed += 1;
                    continue;
                }
                if let Err(e) = stream.finish() {
                    eprintln!("stream finish failed (tx {}): {}", i, e);
                    failed += 1;
                    continue;
                }
                sent += 1;
            }
            Err(e) => {
                eprintln!("open stream failed (tx {}): {}", i, e);
                failed += 1;
            }
        }
    }

    println!(
        "spam complete | target: {target} | requested: {count} | sent: {sent} | failed: {failed}"
    );
    Ok(())
}

fn parse_recipient(recipient: Option<String>, identity: &Arc<Keypair>) -> anyhow::Result<Pubkey> {
    match recipient {
        Some(s) => s
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid recipient pubkey: {}", s)),
        None => Ok(identity.pubkey()),
    }
}

fn default_keypair_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".config/solana/id.json")
}
