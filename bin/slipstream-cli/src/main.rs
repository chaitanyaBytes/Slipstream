use anyhow::Context;
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use slipstream_common::Config;
use slipstream_net::{Cartographer, QuicEngine};
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
    let identity = read_keypair_file(&keypair_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to load keypair from {:?}: {}. use --keypair to specify path",
            keypair_path,
            e
        )
    })?;

    match cli.command {
        Commands::Monitor => monitor_loop(&cfg, &keypair_path.display().to_string()).await?,
        Commands::Fire {
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(cfg.default_priority_fee);
            fire_transaction(&cfg, &identity, to, fee).await?;
        }
        Commands::Spam {
            count,
            recipient,
            priority_fee,
        } => {
            let to = parse_recipient(recipient, &identity)?;
            let fee = priority_fee.unwrap_or(cfg.default_priority_fee);
            spam_transactions(&cfg, &identity, to, count, fee).await?;
        }
    }

    Ok(())
}

async fn monitor_loop(cfg: &Config, keypair_path: &str) -> anyhow::Result<()> {
    println!("[monitor] starting");
    println!("rpc: {}", cfg.rpc_url);
    println!("geyser: {}", cfg.geyser_url.as_deref().unwrap_or("<none>"));
    println!("keypair: {keypair_path}");

    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone()));
    cartographer
        .refresh_topology()
        .await
        .context("failed to refresh topology")?;
    cartographer
        .update_schedule()
        .await
        .context("failed to load leader schedule")?;

    let poll = Duration::from_millis(cfg.rpc_poll_interval_ms);
    let print_interval = Duration::from_millis(cfg.monitor_interval_ms);

    {
        let c = Arc::clone(&cartographer);
        tokio::spawn(async move {
            loop {
                let _ = c.fetch_rpc_slot().await;
                tokio::time::sleep(poll).await;
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

        tokio::time::sleep(print_interval).await;
    }
}

async fn fire_transaction(
    cfg: &Config,
    identity: &Keypair,
    recipient: Pubkey,
    priority_fee: u64,
) -> anyhow::Result<()> {
    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone()));
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
        &[identity],
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
    identity: &Keypair,
    recipient: Pubkey,
    count: u64,
    priority_fee: u64,
) -> anyhow::Result<()> {
    let cartographer = Arc::new(Cartographer::new(cfg.rpc_url.clone()));
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
        &[identity],
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

fn parse_recipient(recipient: Option<String>, identity: &Keypair) -> anyhow::Result<Pubkey> {
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
