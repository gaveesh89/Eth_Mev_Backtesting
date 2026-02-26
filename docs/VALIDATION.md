# Validation Status

## Criterion 1 — DEX-DEX Arbitrage (Directional Correctness)

**Status: PASS**

- Reserve reader validated against live archive RPC (storage-slot decode = `eth_call`)
- Cross-DEX price-difference diagnostics implemented and passing
- USDC depeg stress scanner (`test_arb_scanner_usdc_depeg_stress`): 100 blocks, ~6 opportunities detected
- Determinism gate (`test_reserves_storage_decode_equals_eth_call`): storage-slot decode matches `eth_call getReserves`
- Integer spread comparison: `spread_bps_integer()` — cross-multiplication, no floats

### Run commands

```bash
export MEV_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
RUST_LOG=debug cargo test -p mev-sim test_reserve_values_match_etherscan -- --nocapture
RUST_LOG=debug cargo test -p mev-sim test_cross_dex_price_difference_diagnostic -- --nocapture
RUST_LOG=debug cargo test -p mev-sim test_arb_scanner_usdc_depeg_stress -- --ignored --nocapture
RUST_LOG=debug cargo test -p mev-sim test_reserves_storage_decode_equals_eth_call -- --nocapture
```

---

## Criterion 1 — CEX-DEX Arbitrage (Directional Correctness)

**Status: PASS**

All decision paths use `U256` integer math — no `f64` in spread comparison or profit estimation.

- **Real Binance 1s klines**: 1,698,930 ETHUSDC klines ingested (March 2023) as micro-USD integers
- **Block coverage**: 100/100 blocks in depeg window (16,817,000–16,817,099) have DB price hits
- **Price diversity**: 17 distinct CEX close prices ($1,612.99 – $1,619.72)
- **Profit curve**: Unimodal shape verified — interior peak at ~1% of reserves, descending both sides
- **Spread comparison**: cross-multiplication `spread_numerator * 10000 <= dex_price * fee_bps`
- **CEX price path**: CSV f64 → micro-USD (×10⁶) at intake → `micro_usd_to_cex_price_fp` (×100) → 8-decimal FP. No f64 touches the math engine.
- **Data quality**: `NoCexData` and `StaleCexData` verdicts (configurable via `MEV_CEX_MAX_STALE_SECONDS`, default 60s)

### Test results

| Test | Result |
|------|--------|
| `test_cex_dex_profit_curve_shape` | PASS — unimodal, interior peak at index 21 of 58 candidates |
| `test_cex_dex_with_real_klines` | PASS — 100 blocks, 100 DB hits, 17 CEX prices, 7 opportunities |
| `test_cex_dex_depeg` | PASS — 100 blocks scanned with real Binance prices |
| `cex_integer_threshold_is_stable_under_tight_spread` | PASS — stable verdict under ±1 wei perturbation |
| `cex_verdicts_for_missing_and_stale_data` | PASS — NoCexData + StaleCexData paths covered |

### Run commands

```bash
export MEV_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
# Quick (no RPC needed):
cargo test -p mev-sim cex_integer_threshold -- --nocapture
cargo test -p mev-sim cex_verdicts_for_missing -- --nocapture

# Full (requires archive RPC + data/mev.sqlite with ingested klines):
MEV_DB_PATH=$PWD/data/mev.sqlite cargo test -p mev-sim test_cex_dex_profit_curve_shape -- --ignored --nocapture
MEV_DB_PATH=$PWD/data/mev.sqlite cargo test -p mev-sim test_cex_dex_with_real_klines -- --ignored --nocapture
MEV_DB_PATH=$PWD/data/mev.sqlite cargo test -p mev-sim test_cex_dex_depeg -- --ignored --nocapture

# CEX kline ingestion:
cargo run -- --db-path data/mev.sqlite ingest-cex --csv data/cex/ETHUSDC-1s-2023-03.csv
```

---

## Criterion 1 — DEX-DEX Intra-Block

**Status: PASS (structurally validated, zero-arb in low-activity window is expected)**

Merged timeline of all `Sync` logs sorted by `(tx_index, log_index)`, with `detect_v2_arb_opportunity_with_reason` for reject-reason capture.

- **Integer spread**: `spread_bps_integer()` — cross-multiplication, no floats
- **Candidate triggers**: rows emitted when spread >= 30 bps even if sizing solver rejects (profit=0, verdict populated)
- **Verdict column**: stored in `intra_block_arbs.verdict` for post-hoc analysis

### Interpreting 0 intra arbs

During low-activity periods (e.g., USDC depeg window blocks 16,817,000–16,817,099), tracked pools may have very few Sync events per block (often 0–2). This means reserve states rarely change intra-block, so cross-DEX spreads seen pre-block persist unchanged. Zero intra-block arbs in such windows is **expected behavior**, not a bug.

### Diagnostics

```bash
# Dump timeline for a specific block
MEV_INTRA_DUMP_BLOCK=16817000 MEV_INTRA_DEBUG=1 cargo run -- simulate --block 16817000 --algo dex_dex_intra 2>&1 | grep -E "INTRA_DUMP|intra-block"

# Check sync activity
MEV_INTRA_DEBUG=1 cargo run -- simulate --block 16817000 --algo dex_dex_intra 2>&1 | grep "sync"
```

### Dump mode

Set `MEV_INTRA_DUMP_BLOCK=<block_number>` to dump the first 30 timeline steps for that block, including:
- Which pool changed
- Current reserves for both pools in each pair
- Integer spread (bps)
- Verdict reason from the arb detector

---

## Criterion 2 — Ground-Truth Matching

**Status: PENDING**

- Regression harness test is implemented (`test_criterion_2_known_arb_matching`).
- Data file is `test_data/known_arb_txs.json`.
- Populate with EigenPhi-verified entries to activate full checks.

---

## Instrumentation

```bash
# CEX-DEX per-block telemetry
MEV_CEX_DEBUG=1 cargo run -- simulate --block 16817000 --algo cex_dex

# Intra-block dump mode
MEV_INTRA_DUMP_BLOCK=16817050 MEV_INTRA_DEBUG=1 RUST_LOG=info cargo run -- simulate --block 16817050 --algo dex_dex_intra 2>&1
```
