# MEV Backtest Toolkit

Educational Rust toolkit for replaying historical Ethereum blocks and understanding MEV mechanics through simulation. **Analysis only — this project does NOT submit transactions to any relay, RPC, or network.**

> **11,400+ lines of Rust** across 4 crates, **100 tests**, 3 MEV strategy scanners, 6 CLI subcommands.

## Prerequisites

- **Rust** 1.82+ stable (`rustup default stable`)
- **RPC endpoint** — Alchemy, Infura, or local Reth/Erigon (set via `MEV_RPC_URL`)
- **Disk** — ~2 GB for the SQLite database
- **Optional** — Binance CEX kline CSVs for CEX-DEX spread analysis

## Quick Start

### 1. Clone and Build
```bash
git clone https://github.com/gaveesh89/Eth_Mev_Backtesting.git
cd Eth_Mev_Backtesting
cargo build --release
```

### 2. Fetch On-Chain Blocks
```bash
export MEV_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY

cargo run --release --bin mev-cli -- \
  fetch --start-block 17000000 --end-block 17000010
```

### 3. Ingest CEX Prices (optional)
```bash
cargo run --release --bin mev-cli -- \
  ingest-cex --csv data/ETHUSDT-1s-2023-04-14.csv
```

### 4. Simulate a Block
```bash
cargo run --release --bin mev-cli -- \
  simulate --block 17000000 --algorithm egp
```
Algorithms: `egp` (Effective Gas Price sort), `profit` (MEV-profit sort).

### 5. Analyze Results
```bash
cargo run --release --bin mev-cli -- \
  analyze --start-block 17000000 --end-block 17000010
```

### 6. Diagnose Scanner Coverage
```bash
cargo run --release --bin mev-cli -- \
  diagnose --block 17000000
```
Reports V2/V3 swap event counts, scanned vs unscanned pool coverage, cross-DEX spreads, reserve health, and CEX timestamp alignment.

### 7. Check Database Status
```bash
cargo run --release --bin mev-cli -- status
```

---

## Architecture

```
Ethereum RPC ──fetch──▶ ┌──────────────┐
(Alloy provider)        │   mev-data   │
                        │  SQLite WAL  │◀── ingest-cex ── Binance CSV
Flashbots Parquet ──────▶│  Store layer │
                        └──────┬───────┘
                               │ load txs + reserves
                               ▼
                        ┌──────────────┐
                        │   mev-sim    │
                        │  REVM fork   │
                        │  + Strategies│
                        └──────┬───────┘
                               │ SimResult, MevOpportunity
                               ▼
                        ┌──────────────┐
                        │ mev-analysis │
                        │  P&L, classify│
                        └──────┬───────┘
                               │ BlockPnL, MevClassification
                               ▼
                        ┌──────────────┐
                        │   mev-cli    │
                        │ table/JSON/CSV│
                        └──────────────┘
```

**Pipeline:**
1. **Fetch** — Download block headers, transactions, and receipts via Alloy RPC; optionally download Flashbots mempool Parquet files
2. **Store** — Persist everything in SQLite (WAL mode) with tables for blocks, transactions, mempool, simulation results, MEV opportunities, CEX prices, and intra-block arbs
3. **Simulate** — Fork EVM state at block N-1 via REVM, re-order transactions, execute strategies
4. **Analyze** — Compute P&L capture rates, classify sandwich/arbitrage/backrun patterns
5. **Export** — ASCII tables, JSON, or CSV output

---

## Workspace Structure

```
mev-backtest-toolkit/
├── AGENTS.md                ← coding conventions & boundaries
├── spec.md                  ← architecture reference & schema
├── Cargo.toml               ← workspace root, pinned deps
├── crates/
│   ├── mev-data/            ← data ingestion (SQLite, Parquet, RPC)
│   │   └── src/
│   │       ├── blocks.rs    ← BlockFetcher (Alloy RPC)
│   │       ├── mempool.rs   ← Flashbots Parquet parser
│   │       ├── store.rs     ← SQLite Store (928 lines)
│   │       └── types.rs     ← Block, BlockTransaction, MempoolTransaction
│   ├── mev-sim/             ← simulation engine
│   │   └── src/
│   │       ├── evm.rs       ← EvmFork (REVM 19 state fork)
│   │       ├── decoder.rs   ← V2/V3 swap calldata decoder
│   │       ├── ordering.rs  ← EGP sort, profit sort, nonce constraints
│   │       └── strategies/
│   │           ├── arbitrage.rs      ← Cross-DEX V2 arb scanner
│   │           ├── cex_dex_arb.rs    ← CEX-DEX spread scanner
│   │           └── dex_dex_intra.rs  ← Intra-block DEX-DEX arb
│   ├── mev-analysis/        ← post-simulation analytics
│   │   └── src/
│   │       ├── classify.rs  ← Sandwich/backrun/arb heuristic classifier
│   │       └── pnl.rs       ← P&L computation, capture rates
│   └── mev-cli/             ← binary entry point (clap 4)
│       └── src/main.rs      ← 6 subcommands (~1,120 lines)
├── tests/                   ← workspace-level integration tests
│   ├── analysis.rs
│   ├── arbitrage_correctness.rs
│   └── simulation.rs
├── scripts/                 ← Python diagnostic & research scripts
├── docs/                    ← tutorials, math reference, glossary
├── data/                    ← SQLite database (gitignored)
└── sql/                     ← validation queries
```

---

## CLI Reference

| Subcommand | Description | Key Flags |
|---|---|---|
| `fetch` | Fetch blocks from RPC + optional mempool Parquet download | `--start-block`, `--end-block`, `--date`, `--data-dir` |
| `ingest-cex` | Ingest Binance kline CSVs into SQLite | `--csv` (repeatable) |
| `simulate` | Simulate a single block with ordering algorithm | `--block`, `--algorithm` (`egp`/`profit`) |
| `analyze` | Analyze block range: P&L, MEV classification | `--start-block`, `--end-block`, `--output` |
| `status` | Show database statistics, block counts, simulation state | `--block` (optional, for single block detail) |
| `diagnose` | Audit scanner coverage: V2/V3 events, reserves, spreads | `--block` |

**Global flags:** `--db-path` (default: `data/mev.sqlite`), `-v` (verbose, repeatable), `-q` (quiet)

---

## MEV Strategy Scanners

### 1. Cross-DEX V2 Arbitrage (`arbitrage.rs`)

Compares reserves across Uniswap V2 and SushiSwap V2 pools for the same token pair. Uses the constant-product AMM formula to compute optimal input and net profit after the 0.3% swap fee on both legs.

- **Default pairs**: WETH/USDC, WETH/USDT, WETH/DAI (× 2 DEXes = 6 pools)
- **Fee floor**: ~60 bps (two 0.3% swaps) — spreads below this are unprofitable
- **State**: Reads reserves at block N-1 (pre-block snapshot)

### 2. CEX-DEX Spread Scanner (`cex_dex_arb.rs`)

Compares on-chain DEX mid-price against nearest Binance 1-second kline. Flags opportunities where the spread exceeds a configurable threshold (default: 30 bps).

- **CEX source**: Binance ETHUSDT 1s klines ingested via `ingest-cex`
- **Staleness window**: 3 seconds — CEX prices older than this are rejected
- **Uses same pool set** as the cross-DEX scanner

### 3. Intra-Block DEX-DEX Scanner (`dex_dex_intra.rs`)

Tracks reserve changes within a single block by parsing V2 Sync events from transaction receipts. Builds a merged timeline of reserve states and checks for cross-DEX spread after each update.

- **Detection**: Uses `Sync(uint112,uint112)` events per pool
- **Same 6-pool universe** as cross-DEX scanner

### Known Limitations

All three scanners share structural limitations documented in each module:

- **V2 only** — Uniswap V3, Curve, Balancer pools are not scanned
- **3 token pairs** — Only WETH/{USDC, USDT, DAI}; long-tail altcoin pairs are invisible
- **Pre-block reserves** — Reads state at N-1; misses intra-block state changes (except dex_dex_intra)
- **No multi-hop** — Only 2-pool direct arbs; A→B→C→A cycles are not evaluated
- **Post-PBS baseline** — The builder has already extracted most MEV before our snapshot
- **EVM simulation is a stub** — Returns hardcoded gas; actual REVM execution not yet wired to strategy output
- **Transaction logs are not stored** — Receipts are discarded by `BlockFetcher`; limits offline analysis

The `diagnose` command quantifies these gaps for any given block.

---

## MEV Classification (mev-analysis)

### Sandwich Detector

Rolling 3-transaction window checks:
1. `tx[0]` and `tx[2]` share the same sender; `tx[1]` has a different sender
2. `tx[0]` buys token X, `tx[1]` swaps token X, `tx[2]` sells token X
3. `tx[0]` and `tx[2]` have higher effective gas price than `tx[1]`

Confidence: 0.9 (all 3 conditions), 0.6 (2 of 3). Results are deduplicated by overlapping tx hashes.

### P&L Computation

Computes per-block capture rates: simulated block value vs actual block value. Outputs `BlockPnL` with gas revenue, MEV captured, and ordering efficiency metrics.

---

## Key Concepts

| Concept | Definition |
|---------|-----------|
| MEV | Maximal Extractable Value — profit available from transaction ordering |
| Arbitrage | Two-pool DEX price discrepancy: buy low on pool A, sell high on pool B |
| Sandwich | Front-run + back-run a victim tx to capture price impact |
| EGP | Effective Gas Price: `min(maxFeePerGas, baseFee + priorityFee)` |
| Capture Rate | Simulated value ÷ actual block value |
| REVM | Rust EVM implementation for fast offline execution |

See [docs/mev-concepts-glossary.md](docs/mev-concepts-glossary.md) for full glossary.

---

## Comparison: This Toolkit vs. rbuilder vs. EigenPhi

| Aspect | This Toolkit | rbuilder | EigenPhi |
|--------|--------------|----------|----------|
| **Purpose** | Educational backtest | Production block builder | MEV analytics platform |
| **Language** | Rust | Rust | Proprietary |
| **Detection** | Reserve-based (V2 pools) | Full PBS integration | Transfer-graph SCC (protocol-agnostic) |
| **MEV Types** | Arb, sandwich, backrun | Full spectrum | Arb, sandwich, liquidation, flash loan |
| **Pool Coverage** | 6 hardcoded V2 pools | All pools | All pools + all protocols |
| **Data Source** | Alchemy/Infura RPC | Local Reth | Erigon archive node |
| **Storage** | SQLite (local) | In-memory | Google Cloud |
| **Scope** | Historical replay | Real-time building | Real-time + historical |
| **LOC** | ~11.4k | ~50k | Unknown |
| **License** | MIT | AGPL-3.0 | Proprietary |

---

## Tech Stack

| Dependency | Version | Purpose |
|---|---|---|
| [alloy](https://github.com/alloy-rs/alloy) | 0.12 | Ethereum types & RPC provider |
| [revm](https://github.com/bluealloy/revm) | 19 | EVM simulation engine |
| [rusqlite](https://github.com/rusqlite/rusqlite) | 0.32 | SQLite storage (WAL mode) |
| [parquet](https://github.com/apache/arrow-rs) | 53 | Flashbots mempool file parsing |
| [clap](https://github.com/clap-rs/clap) | 4 | CLI argument parsing |
| [tokio](https://github.com/tokio-rs/tokio) | 1 | Async runtime |
| [eyre](https://github.com/eyre-rs/eyre) | 0.6 | Error handling |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1 | Structured logging |
| [reqwest](https://github.com/seanmonstar/reqwest) | 0.12 | HTTP client |
| [criterion](https://github.com/bheisler/criterion.rs) | 0.5 | Benchmarking |

---

## Performance Reference

Benchmarks (Apple M1, release build):

| Operation | Time |
|-----------|------|
| Sort 100 txs by EGP | **15.5 µs** |
| Apply nonce constraints (50 txs) | **6.4 µs** |
| Detect arbs (10 pool pairs) | **3.2 µs** |
| Single tx simulate (warm REVM state) | **< 5 ms** |
| Fetch 1 block via RPC | **100–500 ms** |
| SQLite batch insert (1k rows) | **< 50 ms** |

~1000 blocks in 10–15 minutes with a local RPC node.

---

## Development

### Verification Gate (must pass before commits)

```bash
cargo check
cargo clippy -- -D warnings
cargo fmt --all
cargo nextest run          # or: cargo test --workspace
```

### Adding a New Strategy

1. Create `crates/mev-sim/src/strategies/my_strategy.rs`
2. Add detection logic returning `eyre::Result<Vec<MevOpportunity>>`
3. Write ≥1 unit test per public function
4. Export from `strategies/mod.rs`
5. Wire into CLI subcommand
6. Run verification gate

### Code Standards

- `eyre::Result` everywhere in library crates — never `unwrap()`
- `tracing` macros only — never `println!`
- `///` docs on all public items; `//!` module-level docs
- `#[tracing::instrument]` on all public async functions

See [AGENTS.md](AGENTS.md) for complete rules.

---

## FAQ

**Q: Why SQLite instead of PostgreSQL?**
A: Zero infrastructure. rbuilder uses the same pattern. Ideal for local historical analysis.

**Q: Can I use this to submit bundles or trade?**
A: No. This is analysis-only. It does not connect to any relay or submit transactions.

**Q: Why does the scanner find zero opportunities on some blocks?**
A: The default scanner covers only 6 V2 pools (WETH/{USDC,USDT,DAI} on UniV2 + Sushi). Most real MEV activity occurs on V3 pools and long-tail pairs outside this set. Use `diagnose --block N` to quantify coverage gaps. In post-PBS Ethereum, the block builder has already extracted most residual arb before our snapshot.

**Q: What is the `diagnose` command?**
A: It audits a block's swap activity — counts V2/V3 events, splits them by scanned vs unscanned pools, reads reserves, computes cross-DEX spreads, and checks CEX timestamp alignment. It tells you *why* the scanner did or didn't find opportunities.

---

## Resources

- [spec.md](spec.md) — Architecture reference, schema, addresses, arb math
- [AGENTS.md](AGENTS.md) — Coding conventions & boundary definitions
- [docs/mev-concepts-glossary.md](docs/mev-concepts-glossary.md) — MEV terminology
- [docs/tutorial-1-first-backtest.md](docs/tutorial-1-first-backtest.md) — First backtest walkthrough
- [docs/tutorial-2-arb-math.md](docs/tutorial-2-arb-math.md) — Arbitrage math deep dive
- [docs/MATH.md](docs/MATH.md) — Optimal input derivation
- [docs/VALIDATION.md](docs/VALIDATION.md) — Validation methodology

---

## License

MIT — Use freely for educational and research purposes.

