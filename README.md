# MEV Backtest Toolkit

Educational Rust toolkit for replaying historical Ethereum blocks and understanding MEV mechanics through simulation. **Analysis only — this project does NOT submit transactions to any relay, RPC, or network.**

> **16,200+ lines of Rust** across 5 crates, **146 tests**, 3 MEV strategy scanners, EigenPhi-style SCC classification, 9 CLI subcommands, and a [live WASM dashboard](https://gaveesh89.github.io/Eth_Mev_Backtesting/).

## Prerequisites

- **Rust** 1.82+ stable (`rustup default stable`)
- **RPC endpoint** — Alchemy, Infura, or local Reth/Erigon (set via `MEV_RPC_URL`)
- **Disk** — ~2 GB for the SQLite database
- **Optional** — `trunk` for building the WASM dashboard locally

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

### 3. Fetch CEX Prices (automated from Binance API)
```bash
cargo run --release --bin mev-cli -- \
  fetch-cex --start-block 17000000 --end-block 17000010
```
No API key required — uses the public `data-api.binance.vision` endpoint with automatic pagination and rate-limit handling. Alternatively, ingest from CSV:
```bash
cargo run --release --bin mev-cli -- \
  ingest-cex --csv data/ETHUSDT-1s-2023-04-14.csv
```

### 4. Simulate a Block
```bash
cargo run --release --bin mev-cli -- \
  simulate --block 17000000 --algorithm egp
```
Algorithms: `egp` (Effective Gas Price sort), `profit` (MEV-profit sort), `arbitrage`, `cex_dex`, `dex_dex_intra`, `both`.

### 5. Analyze Results
```bash
cargo run --release --bin mev-cli -- \
  analyze --start-block 17000000 --end-block 17000010
```

### 6. Classify MEV (EigenPhi-style SCC detection)
```bash
cargo run --release --bin mev-cli -- \
  classify --start-block 17000000 --end-block 17000010 --output table
```
Builds a directed ERC-20 transfer graph per transaction, runs Tarjan's SCC algorithm, detects cyclic arbitrage flows protocol-agnostically.

### 7. Read Uniswap V3 Pool Price
```bash
MEV_ENABLE_V3=1 cargo run --release --bin mev-cli -- \
  v3-price --pool 0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8 --block 18000000
```
Reads `slot0()` via both `eth_call` and `eth_getStorageAt`, cross-validates, and converts `sqrtPriceX96` to a human-readable price using integer-only math.

### 8. Diagnose Scanner Coverage
```bash
cargo run --release --bin mev-cli -- \
  diagnose --block 17000000
```
Reports V2/V3 swap event counts, scanned vs unscanned pool coverage, cross-DEX spreads, reserve health, and CEX timestamp alignment.

### 9. Check Database Status
```bash
cargo run --release --bin mev-cli -- status
```

---

## WASM Dashboard

A browser-based dashboard that runs entirely in WebAssembly — no server required. Try it live:

**[https://gaveesh89.github.io/Eth_Mev_Backtesting/](https://gaveesh89.github.io/Eth_Mev_Backtesting/)**

Features:
- **Configure**: Alchemy, Infura, or custom RPC — enter API key and go
- **Preset block ranges**: USDC Depeg, The Merge, High Gas, Quick Test
- **Strategy selection**: Full MEV scan, DEX-DEX arbitrage, or sandwich detection
- **Live progress**: Progress bar and log while blocks are fetched + analyzed
- **Results**: Summary cards, opportunities table with Etherscan links, ordering analysis, block breakdown
- **Export**: Download results as JSON

Built with Yew 0.21, compiles to 558 KB WASM. Run locally:
```bash
cd crates/mev-dashboard
trunk serve   # → http://127.0.0.1:8080
```

---

## Architecture

```
                        ┌───────────────────────────────────────────┐
                        │              mev-dashboard                │
                        │         (WASM · Yew · GitHub Pages)       │
                        │         Browser-based MEV analysis        │
                        └───────────────────┬───────────────────────┘
                                            │ fetches via browser RPC
                                            ▼
Ethereum RPC ──fetch──▶ ┌──────────────┐
(Alloy provider)        │   mev-data   │
                        │  SQLite WAL  │◀── fetch-cex ── Binance API
Flashbots Parquet ──────▶│  Store layer │◀── ingest-cex ── CSV files
                        └──────┬───────┘
                               │ load txs + reserves
                               ▼
                        ┌──────────────┐
                        │   mev-sim    │
                        │  REVM fork   │
                        │  + Strategies│
                        │  + V3 reader │
                        └──────┬───────┘
                               │ SimResult, MevOpportunity
                               ▼
                        ┌──────────────┐
                        │ mev-analysis │
                        │  P&L, SCC,   │
                        │  classify    │
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
2. **CEX Data** — Fetch Binance klines via API (`fetch-cex`) or ingest from CSV — stored as micro-USD integers
3. **Store** — Persist in SQLite (WAL mode) across 7 tables: `blocks`, `block_transactions`, `mempool_transactions`, `simulation_results`, `mev_opportunities`, `cex_prices`, `intra_block_arbs`
4. **Simulate** — Fork EVM state at block N-1 via REVM, re-order transactions, execute strategy scanners
5. **Classify** — Build ERC-20 transfer graphs, run Tarjan SCC detection, compute net balances
6. **Analyze** — Compute P&L capture rates, sandwich/arbitrage/backrun heuristics
7. **Export** — ASCII tables, JSON, or CSV output

---

## Workspace Structure

```
mev-backtest-toolkit/
├── AGENTS.md                 ← coding conventions & boundaries
├── spec.md                   ← architecture reference & schema
├── Cargo.toml                ← workspace root, 22 pinned deps
├── crates/
│   ├── mev-data/             ← data ingestion (SQLite, Parquet, RPC, Binance API)
│   │   └── src/
│   │       ├── blocks.rs     ← BlockFetcher (Alloy RPC)
│   │       ├── cex.rs        ← Binance kline fetcher (pagination, rate-limits)
│   │       ├── mempool.rs    ← Flashbots Parquet parser
│   │       ├── store.rs      ← SQLite Store (7 tables, migrations, WAL)
│   │       └── types.rs      ← Block, BlockTransaction, MempoolTransaction, TxLog
│   ├── mev-sim/              ← simulation engine (8,400+ lines)
│   │   └── src/
│   │       ├── evm.rs        ← EvmFork (REVM 19 state fork)
│   │       ├── decoder.rs    ← V2/V3 swap calldata decoder (16 tests)
│   │       ├── ordering.rs   ← EGP sort, profit sort, nonce constraints
│   │       ├── strategies/
│   │       │   ├── arbitrage.rs      ← Cross-DEX V2 arb scanner
│   │       │   ├── cex_dex_arb.rs    ← CEX-DEX spread scanner
│   │       │   └── dex_dex_intra.rs  ← Intra-block DEX-DEX arb
│   │       └── v3/
│   │           ├── mod.rs    ← Feature gate (MEV_ENABLE_V3)
│   │           ├── slot0.rs  ← V3 slot0 reader (eth_call + storage)
│   │           └── price.rs  ← sqrtPriceX96 → human price (integer math)
│   ├── mev-analysis/         ← post-simulation analytics
│   │   └── src/
│   │       ├── classify.rs       ← Sandwich/backrun/arb heuristic classifier
│   │       ├── pnl.rs            ← P&L computation, capture rates
│   │       ├── scc_detector.rs   ← Tarjan SCC arbitrage detection (EigenPhi-inspired)
│   │       └── transfer_graph.rs ← Directed ERC-20 transfer graph construction
│   ├── mev-cli/              ← binary entry point (clap 4)
│   │   └── src/main.rs       ← 9 subcommands (~1,590 lines)
│   └── mev-dashboard/        ← WASM dashboard (Yew, GitHub Pages)
│       ├── src/
│       │   ├── main.rs       ← App shell, state machine
│       │   ├── analysis.rs   ← Browser-side MEV detection
│       │   ├── rpc.rs        ← JSON-RPC via browser fetch
│       │   ├── types.rs      ← Shared types, presets
│       │   └── components/   ← Setup, Running, Results UI
│       ├── style.css         ← Dark theme, responsive
│       └── Trunk.toml        ← WASM build config
├── tests/                    ← workspace-level integration tests
│   ├── analysis.rs
│   ├── arbitrage_correctness.rs
│   └── simulation.rs
├── docs/                     ← tutorials, math reference, glossary
├── data/                     ← SQLite database (gitignored)
└── .github/workflows/
    ├── ci.yml                ← cargo check → clippy → fmt → test
    └── dashboard.yml         ← Trunk build → deploy to GitHub Pages
```

---

## CLI Reference

| Subcommand | Description | Key Flags |
|---|---|---|
| `fetch` | Fetch blocks from RPC + optional mempool Parquet download | `--start-block`, `--end-block`, `--date`, `--data-dir` |
| `fetch-cex` | Fetch Binance klines via public API for a block range | `--start-block`, `--end-block`, `--pair` (ETHUSDC), `--interval` (1s), `--padding-seconds` |
| `ingest-cex` | Ingest Binance kline CSVs into SQLite | `--csv` (repeatable) |
| `simulate` | Simulate a block with ordering algorithm or strategy scanner | `--block`, `--algorithm` (`egp`/`profit`/`arbitrage`/`cex_dex`/`dex_dex_intra`/`both`) |
| `analyze` | Analyze block range: P&L, MEV classification | `--start-block`, `--end-block`, `--output` (`table`/`json`/`csv`) |
| `classify` | SCC-based transfer-graph MEV classification | `--start-block`, `--end-block`, `--output` (`table`/`json`) |
| `diagnose` | Audit scanner coverage: V2/V3 events, reserves, spreads | `--block` |
| `v3-price` | Read Uniswap V3 pool price at historical block | `--pool`, `--block`, `--token0-decimals`, `--token1-decimals` |
| `status` | Show database statistics, block counts, simulation state | `--block` (optional) |

**Global flags:** `--db-path` (default: `data/mev.sqlite`), `-v` (verbose, repeatable), `-q` (quiet)

---

## MEV Strategy Scanners

### 1. Cross-DEX V2 Arbitrage (`arbitrage.rs`)

Compares reserves across Uniswap V2 and SushiSwap V2 pools for the same token pair. Uses the constant-product AMM formula with closed-form optimal input and ternary search fallback.

- **Default pairs**: WETH/USDC, WETH/USDT, WETH/DAI (× 2 DEXes = 6 pools)
- **Integer math**: `spread_bps_integer()` — cross-multiplication, no floats
- **Fee floor**: ~60 bps (two 0.3% swaps) — spreads below this are unprofitable
- **Configurable**: `MEV_ARB_GAS_UNITS`, `MEV_ARB_MIN_DISCREPANCY_BPS`, `MEV_ARB_MIN_PROFIT_WEI`

### 2. CEX-DEX Spread Scanner (`cex_dex_arb.rs`)

Compares on-chain DEX mid-price against nearest Binance 1-second kline. Flags opportunities where the spread exceeds a configurable threshold.

- **CEX source**: Binance klines via API (`fetch-cex`) or CSV (`ingest-cex`) — stored as micro-USD integers
- **Integer math**: `micro_usd_to_cex_price_fp` (×10⁸ fixed-point) — no `f64` touches the spread comparison
- **Staleness**: Configurable via `MEV_CEX_MAX_STALE_SECONDS` (default 60s); rejects stale data with `StaleCexData` verdict
- **Verdicts**: `NoCexData`, `StaleCexData`, `SpreadBelowFee`, `NonPositiveProfit`, `Opportunity`

### 3. Intra-Block DEX-DEX Scanner (`dex_dex_intra.rs`)

Tracks reserve changes within a single block by parsing V2 `Sync` events from transaction receipts. Builds a merged timeline of reserve states sorted by `(tx_index, log_index)`.

- **Detection**: Re-evaluates all pairs after each `Sync` event
- **Verdict column**: Stored in `intra_block_arbs.verdict` for post-hoc analysis
- **Dump mode**: Set `MEV_INTRA_DUMP_BLOCK=<N>` to dump the first 30 timeline steps

### Known Limitations

All three scanners share structural limitations documented in each module:

- **V2 only** — Uniswap V3, Curve, Balancer pools are not scanned (V3 price reader is a proof-of-concept)
- **3 token pairs** — Only WETH/{USDC, USDT, DAI}; long-tail altcoin pairs are invisible
- **Pre-block reserves** — Reads state at N-1; misses intra-block changes (except `dex_dex_intra`)
- **No multi-hop** — Only 2-pool direct arbs; A→B→C→A cycles are not evaluated
- **Post-PBS baseline** — The builder has already extracted most MEV before our snapshot

The `diagnose` command quantifies these gaps for any given block.

---

## MEV Classification (mev-analysis)

### SCC-Based Arbitrage Detection (EigenPhi-inspired)

Protocol-agnostic arbitrage detection using Tarjan's strongly connected component algorithm:

1. Parse all ERC-20 `Transfer` events in a transaction
2. Build a directed graph (`TxTransferGraph`) of token flows between addresses
3. Run Tarjan's SCC to find cyclic subgraphs (>1 node)
4. Locate the closest node to `tx.from`/`tx.to` in the SCC
5. Compute SCC-internal net balance per token
6. Any token with positive net balance → arbitrage detected

This approach detects arbs across *any* DEX protocol without needing protocol-specific decoders.

### Sandwich Detector

Rolling 3-transaction window checks:
1. `tx[0]` and `tx[2]` share the same sender; `tx[1]` has a different sender
2. `tx[0]` buys token X, `tx[1]` swaps token X, `tx[2]` sells token X
3. `tx[0]` and `tx[2]` have higher effective gas price than `tx[1]`

Confidence: 0.95 (all conditions), 0.8 (3 of 4), 0.5 (2 of 4). Results deduplicated by overlapping tx hashes.

### P&L Computation

Computes per-block capture rates: simulated block value vs actual block value. Outputs `BlockPnL` with gas revenue, MEV captured, private flow estimates, and ordering efficiency metrics. Aggregates into `RangeStats` across block ranges.

---

## Validation Status

| Area | Status | Evidence |
|------|--------|----------|
| DEX-DEX Arbitrage | **PASS** | Reserve reader validated against archive RPC, integer spread, USDC depeg stress test (100 blocks, ~6 opps) |
| CEX-DEX Arbitrage | **PASS** | 1.7M klines ingested, 100/100 blocks with DB hits, 17 distinct CEX prices, unimodal profit curve |
| Intra-Block DEX-DEX | **PASS** | Merged Sync timeline, integer spread, zero-arb in low-activity windows expected |
| V3 Price Reader | **PASS** | Both `eth_call` and `eth_getStorageAt`, cross-validated, integer-only math |
| Automated CEX Fetch | **PASS** | Binance public API, pagination, rate-limits, 5 unit tests |
| Ground-Truth Matching | **PENDING** | Regression harness implemented, awaiting EigenPhi-verified entries |

See [docs/VALIDATION.md](docs/VALIDATION.md) for full methodology and test commands.

---

## Key Concepts

| Concept | Definition |
|---------|-----------|
| MEV | Maximal Extractable Value — profit available from transaction ordering |
| Arbitrage | Two-pool DEX price discrepancy: buy low on pool A, sell high on pool B |
| Sandwich | Front-run + back-run a victim tx to capture price impact |
| SCC | Strongly Connected Component — cyclic subgraph indicating round-trip token flows |
| EGP | Effective Gas Price: `min(maxFeePerGas, baseFee + priorityFee)` |
| Capture Rate | Simulated value ÷ actual block value |
| REVM | Rust EVM implementation for fast offline execution |

See [docs/mev-concepts-glossary.md](docs/mev-concepts-glossary.md) for full glossary.

---

## Comparison: This Toolkit vs. rbuilder vs. EigenPhi

| Aspect | This Toolkit | rbuilder | EigenPhi |
|--------|--------------|----------|----------|
| **Purpose** | Educational backtest | Production block builder | MEV analytics platform |
| **Language** | Rust + WASM | Rust | Proprietary |
| **Detection** | Reserve-based + SCC transfer graph | Full PBS integration | Transfer-graph SCC (protocol-agnostic) |
| **MEV Types** | Arb, sandwich, backrun | Full spectrum | Arb, sandwich, liquidation, flash loan |
| **Pool Coverage** | 6 hardcoded V2 pools + V3 reader | All pools | All pools + all protocols |
| **Data Source** | Alchemy/Infura RPC + Binance API | Local Reth | Erigon archive node |
| **Storage** | SQLite (local, 7 tables) | In-memory | Google Cloud |
| **Frontend** | WASM dashboard (GitHub Pages) | None | Web app |
| **Scope** | Historical replay | Real-time building | Real-time + historical |
| **LOC** | ~16.2k | ~50k | Unknown |
| **License** | MIT | AGPL-3.0 | Proprietary |

---

## Tech Stack

| Dependency | Version | Purpose |
|---|---|---|
| [alloy](https://github.com/alloy-rs/alloy) | 0.12 | Ethereum types & RPC provider |
| [revm](https://github.com/bluealloy/revm) | 19 | EVM simulation engine |
| [rusqlite](https://github.com/rusqlite/rusqlite) | 0.32 | SQLite storage (WAL mode) |
| [parquet](https://github.com/apache/arrow-rs) / [arrow](https://github.com/apache/arrow-rs) | 53 | Flashbots mempool file parsing |
| [petgraph](https://github.com/petgraph/petgraph) | 0.6 | Directed graph + Tarjan SCC |
| [clap](https://github.com/clap-rs/clap) | 4 | CLI argument parsing |
| [tokio](https://github.com/tokio-rs/tokio) | 1 | Async runtime |
| [eyre](https://github.com/eyre-rs/eyre) | 0.6 | Error handling |
| [tracing](https://github.com/tokio-rs/tracing) | 0.1 | Structured logging |
| [reqwest](https://github.com/seanmonstar/reqwest) | 0.12 | HTTP client (RPC + Binance API) |
| [yew](https://github.com/yewstack/yew) | 0.21 | WASM frontend framework |
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

## Test Suite

**146 tests** across 21 test files:

| Crate | Tests | Coverage Areas |
|-------|-------|----------------|
| mev-sim | 86 | Strategies (arb, cex-dex, intra-block), decoder (16 selectors), ordering, EVM, V3 reader, fixtures |
| mev-analysis | 24 | SCC detection, transfer graph, sandwich classifier, P&L |
| mev-data | 16 | SQLite store, Binance kline parser, mempool |
| mev-dashboard | 6 | Browser analysis engine |
| integration (tests/) | 14 | End-to-end analysis, arb correctness, simulation |

```bash
cargo nextest run          # all 146 tests
cargo test -p mev-sim      # sim crate only
cargo test -p mev-analysis # analysis crate only
```

---

## Development

### Verification Gate (must pass before commits)

```bash
cargo check
cargo clippy -- -D warnings
cargo fmt --all
cargo nextest run          # or: cargo test --workspace
```

### Building the Dashboard

```bash
cd crates/mev-dashboard
rustup target add wasm32-unknown-unknown
cargo install trunk
trunk serve          # dev server at http://127.0.0.1:8080
trunk build --release  # → dist/ (558 KB WASM)
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

**Q: What is the `classify` command?**
A: It builds a directed ERC-20 transfer graph for every transaction in a block range, runs Tarjan's SCC algorithm to find cyclic token flows, and reports arbitrage detections with profiteer addresses and profit amounts — protocol-agnostically, like EigenPhi.

**Q: Do I need an API key for CEX data?**
A: No. The `fetch-cex` command uses Binance's public `data-api.binance.vision` endpoint (no API key, weight 2 per request). It auto-paginates and handles rate limits.

---

## Resources

- [spec.md](spec.md) — Architecture reference, schema, addresses, arb math
- [AGENTS.md](AGENTS.md) — Coding conventions & boundary definitions
- [docs/VALIDATION.md](docs/VALIDATION.md) — Validation methodology & test results
- [docs/mev-concepts-glossary.md](docs/mev-concepts-glossary.md) — MEV terminology
- [docs/tutorial-1-first-backtest.md](docs/tutorial-1-first-backtest.md) — First backtest walkthrough
- [docs/tutorial-2-arb-math.md](docs/tutorial-2-arb-math.md) — Arbitrage math deep dive
- [docs/MATH.md](docs/MATH.md) — Optimal input derivation
- [crates/mev-dashboard/README.md](crates/mev-dashboard/README.md) — Dashboard build & deploy guide

---

## License

MIT — Use freely for educational and research purposes.

