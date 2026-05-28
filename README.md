# Slipstream

Slipstream is a high-performance Solana transaction client that bypasses traditional RPC-based submission by sending transactions **directly to validator leaders over QUIC**. By leveraging Solana's stake-weighted Quality of Service (swQoS), Slipstream delivers lower latency and higher throughput for time-sensitive operations.

Built for MEV searchers, traders, and anyone needing the absolute lowest latency for Solana transaction submission.

## Quick Start

```bash
cargo build --release
cp .env.example .env
```

Set at least:
- `SOLANA_RPC_URL`
- optional: `GEYSER_URL`

Run:

```bash
cargo run --release -- monitor
cargo run --release -- fire --recipient <PUBKEY> --priority-fee 100000
cargo run --release -- spam --recipient <PUBKEY> --count 10 --priority-fee 100000
```

## Shield Blocklist

Slipstream Shield lets you block malicious validators from target selection.

1. Create a blocklist file (default path `./blocklist.txt`):

```bash
echo "VALIDATOR_PUBKEY_HERE" >> blocklist.txt
```

2. Optional env config:
- `SLIPSTREAM_BLOCKLIST_FILE` (default: `./blocklist.txt`)
- `SLIPSTREAM_BLOCKLIST_URL` (optional remote source)
- `SLIPSTREAM_BLOCKLIST_REFRESH_SECS` (default: `300`)

Behavior:
- blocked validators are skipped in live target resolution
- scout pre-warming also skips blocked validators
- local file is loaded at startup, then refreshed in background
