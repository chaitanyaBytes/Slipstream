use anyhow::Context;
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use slipstream_common::Config;
use slipstream_net::Cartographer;
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

    let keypair_path = cli
        .keypair
        .unwrap_or_else(default_keypair_path)
        .display()
        .to_string();

    match cli.command {
        Commands::Monitor => monitor_loop(&cfg, &keypair_path).await?,
        Commands::Fire {
            recipient,
            priority_fee,
        } => {
            println!("[fire] dry-run");
            println!("rpc: {}", cfg.rpc_url);
            println!("recipient: {}", recipient.as_deref().unwrap_or("<self>"));
            println!(
                "priority_fee: {}",
                priority_fee.unwrap_or(cfg.default_priority_fee)
            );
        }
        Commands::Spam {
            count,
            recipient,
            priority_fee,
        } => {
            println!("[spam] dry-run");
            println!("rpc: {}", cfg.rpc_url);
            println!("count: {count}");
            println!("recipient: {}", recipient.as_deref().unwrap_or("<self>"));
            println!(
                "priority_fee: {}",
                priority_fee.unwrap_or(cfg.default_priority_fee)
            );
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

fn default_keypair_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".config/solana/id.json")
}
