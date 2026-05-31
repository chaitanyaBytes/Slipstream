# Slipstream

## Summary

Slipstream is a high-performance Solana transaction client that bypasses traditional RPC-based submission by sending transactions **directly to validator leaders over QUIC**. By leveraging Solana's stake-weighted Quality of Service (swQoS), Slipstream delivers lower latency and higher throughput for time-sensitive operations.

Built for MEV searchers, traders, and anyone needing the absolute lowest latency for Solana transaction submission.

## Latest Benchmark Snapshot (Devnet)


| Path        | Samples | Avg (ms) | p50 (ms) | p95 (ms) | p99 (ms) |
| ----------- | ------- | -------- | -------- | -------- | -------- |
| Direct QUIC | 20      | 0.02     | 0.02     | 0.03     | 0.03     |
| RPC Submit  | 20      | 130.05   | 103.80   | 203.26   | 313.29   |


Direct QUIC shows much lower client-side submission overhead than RPC round-trip submission in this run.

## Performance


| Optimization                   | Description                                                                                                |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| **Lock-free Connection Cache** | Uses `dashmap` for concurrent access without mutex contention                                              |
| **QUIC Stream Multiplexing**   | Reuses single connection with parallel unidirectional streams, avoiding per-transaction handshake overhead |
| **Connection Pre-warming**     | Scout task maintains hot connections to upcoming leaders, eliminating QUIC handshake latency               |
| **Atomic Slot Tracking**       | `AtomicU64` for slot updates with no locks on reads                                                        |
| **Exponential Backoff**        | Graceful Geyser reconnection with capped exponential backoff                                               |


## Features

- **Direct QUIC Transmission** — Send transactions directly to validator TPU ports via QUIC with Ed25519 identity authentication
- **Dual Clock Modes** — Hybrid mode using Yellowstone Geyser gRPC for real-time slot updates, or legacy RPC polling fallback
- **Leader Schedule Awareness** — Cartographer fetches and caches cluster topology and leader schedules per epoch
- **Connection Pre-warming** — Scout pre-establishes connections to upcoming leaders with configurable lookahead
- **Validator Blocklist (Shield)** — Filter out malicious validators with hot-reloadable blocklist
- **High-Frequency Spam** — Machine gun optimization for rapid transaction submission

## Quick Start

### Build

```bash
cargo build --release
```

### Environment Setup

```bash
export SOLANA_RPC_URL="https://api.mainnet-beta.solana.com"
export GEYSER_URL="your-geyser-grpc-endpoint"  # Optional, enables hybrid mode
```

### Commands

```bash
# Monitor current slot and leader
cargo run --release -- monitor

# Send a single transaction
cargo run --release -- fire --recipient <PUBKEY> --priority-fee 100000

# Spam multiple transactions
cargo run --release -- spam --recipient <PUBKEY> --count 10 --priority-fee 100000
```

### CLI Options

```
cargo run --release -- [OPTIONS] <COMMAND>

# Or after building, run directly:
# ./target/release/slipstream-cli [OPTIONS] <COMMAND>

Commands:
  monitor    Continuously display current slot and leader IP
  fire       Send a single transaction to the current leader
  spam       Send multiple transactions in rapid succession

Options:
  -r, --rpc <URL>           Override RPC endpoint
      --geyser <URL>        Override Geyser gRPC endpoint
  -k, --keypair <PATH>      Path to keypair (default: ~/.config/solana/id.json)

Fire/Spam Options:
      --recipient <PUBKEY>  Recipient pubkey (default: self-transfer)
      --priority-fee <FEE>  Priority fee in microlamports
  -c, --count <N>           Number of transactions (spam only, default: 10)
```

## Project Structure

```
slipstream/
├── bin/
│   └── slipstream-cli/       # CLI entrypoint, command parsing, orchestration
│       └── src/
│           └── main.rs
├── crates/
│   ├── slipstream-net/       # Network layer
│   │   └── src/
│   │       ├── engine.rs       # QUIC connection management
│   │       ├── geyser.rs       # Yellowstone Geyser integration
│   │       ├── cartographer.rs # Leader schedule & cluster topology
│   │       └── blocklist.rs    # Validator blocklist (Shield)
│   └── slipstream-common/    # Shared utilities
│       └── src/
│           ├── config.rs       # Configuration & environment parsing
│           ├── identity.rs     # QUIC certificate generation from keypair
│           └── error.rs        # Error types
└── Cargo.toml
```

## Configuration


| Variable                            | Default                               | Description                                            |
| ----------------------------------- | ------------------------------------- | ------------------------------------------------------ |
| `SOLANA_RPC_URL`                    | `https://api.mainnet-beta.solana.com` | RPC endpoint                                           |
| `GEYSER_URL`                        | —                                     | Yellowstone Geyser gRPC endpoint (enables hybrid mode) |
| `RPC_POLL_INTERVAL_MS`              | `400`                                 | Slot polling interval (legacy mode)                    |
| `SCOUT_INTERVAL_MS`                 | `1000`                                | Connection pre-warming interval                        |
| `SCOUT_LOOKAHEAD_SLOTS`             | `10`                                  | Slots ahead to pre-warm connections                    |
| `MONITOR_INTERVAL_MS`               | `400`                                 | Monitor display refresh rate                           |
| `QUIC_KEEP_ALIVE_SECS`              | `5`                                   | QUIC keep-alive interval                               |
| `QUIC_IDLE_TIMEOUT_SECS`            | `10`                                  | QUIC connection idle timeout                           |
| `DEFAULT_COMPUTE_UNIT_LIMIT`        | `200000`                              | Compute budget per transaction                         |
| `DEFAULT_PRIORITY_FEE`              | `100000`                              | Priority fee in microlamports                          |
| `SLIPSTREAM_BLOCKLIST_FILE`         | `./blocklist.txt`                     | Local blocklist file path                              |
| `SLIPSTREAM_BLOCKLIST_URL`          | —                                     | Optional remote blocklist URL                          |
| `SLIPSTREAM_BLOCKLIST_REFRESH_SECS` | `300`                                 | Blocklist reload interval (seconds)                    |


## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                       slipstream-cli                        │
│  (CLI parsing, command dispatch, async runtime)             │
└─────────────────────────────────────────────────────────────┘
                            │
            ┌───────────────┼───────────────┐
            ▼               ▼               ▼
┌───────────────────┐ ┌───────────┐ ┌─────────────────┐
│   Cartographer    │ │  Engine   │ │ GeyserListener  │
│ (Leader Schedule) │ │  (QUIC)   │ │ (Slot Stream)   │
└───────────────────┘ └───────────┘ └─────────────────┘
            │               │               │
            └───────────────┼───────────────┘
                            ▼
                ┌───────────────────────┐
                │    slipstream-common  │
                │ (Config, Identity,    │
                │  Error Types)         │
                └───────────────────────┘
```

<img width="1672" height="941" alt="Image" src="https://github.com/user-attachments/assets/e7d7c4cc-d808-481e-bc9b-48921c2e7d9a" />


## Validator Blocklist (Shield)

Slipstream Shield protects against malicious validators by filtering them from leader selection.

### Quick Setup

1. Create `blocklist.txt` in the project root:
  ```bash
   # Add one validator pubkey per line
   echo "MALICIOUS_VALIDATOR_PUBKEY_HERE" >> blocklist.txt
  ```
2. Slipstream automatically loads and hot-reloads the file every 5 minutes

### Features

- **Zero-latency filtering** – O(1) lookup with non-blocking concurrent reads
- **Hot-reload** – Updates every 5 minutes without restart
- **Local-first** – No external dependencies by default
- **Optional remote sync** – Set `SLIPSTREAM_BLOCKLIST_URL` for community blocklists

### How It Works

When resolving the current leader, Cartographer checks the blocklist:

- If leader is blocked → returns `None` (transaction skipped)
- If leader is clean → returns socket address (transaction sent)

Scout pre-warming also filters blocked validators to save resources.

See [explanation.md#updates](explanation.md#updates) for detailed architecture and implementation.

## Contributing

Open an issue or submit a pull request.

## Latency Results Interpretation

When you run `compare-latency`, the two paths measure different scopes:

- `Direct QUIC` measures **client-side submit overhead** (writing tx bytes to a QUIC stream).
- `RPC Submit` measures **RPC request/response round-trip latency** (network + RPC server processing).

So it is expected that `Direct QUIC` appears much smaller than `RPC Submit`.

Important:

- These numbers are **not confirmation/finality latency**.
- They are submission-path latency measurements.

Recommended benchmark practice:

1. Run on devnet with a funded wallet.
2. Use self-transfer recipient first.
3. Use low priority fee for cleaner comparisons.
4. Report success rate with latency percentiles (`p50/p95/p99`).

Example:

```bash
SOLANA_RPC_URL="https://api.devnet.solana.com" \
RUST_LOG=info \
cargo run --release -- compare-latency \
  --iterations 20 \
  --recipient <YOUR_PUBKEY> \
  --priority-fee 1 \
  --skip-rpc-preflight
```

