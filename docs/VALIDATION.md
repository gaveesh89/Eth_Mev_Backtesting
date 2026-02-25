# Validation Status

## Criterion 1

- Reserve reader path is validated against live RPC.
- Cross-DEX price-difference diagnostics are implemented and passing.
- USDC depeg stress scanner test exists (`test_arb_scanner_usdc_depeg_stress`) and is marked `#[ignore]` because it requires archive RPC and takes longer.
- Determinism gate test exists (`test_reserves_storage_decode_equals_eth_call`) and verifies `eth_getStorageAt` decode equals `eth_call getReserves` at historical blocks.

### Run commands

```bash
export MEV_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
RUST_LOG=debug cargo test -p mev-sim test_reserve_values_match_etherscan -- --nocapture
RUST_LOG=debug cargo test -p mev-sim test_cross_dex_price_difference_diagnostic -- --nocapture
RUST_LOG=debug cargo test -p mev-sim test_arb_scanner_usdc_depeg_stress -- --ignored --nocapture
RUST_LOG=debug cargo test -p mev-sim test_reserves_storage_decode_equals_eth_call -- --nocapture
```

## Criterion 2 (Ground-truth matching)

- Regression harness test is implemented (`test_criterion_2_known_arb_matching`).
- Data file is `test_data/known_arb_txs.json`.
- Populate with EigenPhi-verified entries to activate full checks.

## CEX-DEX Strategy

All decision paths use `U256` integer math — no `f64` in spread comparison or profit estimation.

- **Spread comparison**: cross-multiplication `spread_numerator * 10000 <= dex_price * fee_bps`
- **CEX price**: 8-decimal fixed-point (`CexPricePoint.close_price_fp`)
- **Data quality**: `NoCexData` and `StaleCexData` verdicts (configurable via `MEV_CEX_MAX_STALE_SECONDS`, default 60s)
- **Instrumentation**: `MEV_CEX_DEBUG=1` enables per-block pricing telemetry

```bash
MEV_CEX_DEBUG=1 cargo run -- simulate --block 16817000 --algo cex_dex
```

## DEX-DEX Intra-Block

Merged timeline of all `Sync` logs sorted by `(tx_index, log_index)`, with `detect_v2_arb_opportunity_with_reason` for reject-reason capture.

- **Integer spread**: `spread_bps_integer()` — cross-multiplication, no floats
- **Candidate triggers**: rows emitted when spread >= 30 bps even if sizing solver rejects (profit=0, verdict populated)
- **Verdict column**: stored in `intra_block_arbs.verdict` for post-hoc analysis

### Interpreting 0 intra arbs

During low-activity periods (e.g., USDC depeg window blocks 16817000–16817099), tracked pools may have very few Sync events per block (often 0–2). This means reserve states rarely change intra-block, so cross-DEX spreads seen pre-block persist unchanged. Zero intra-block arbs in such windows is **expected behavior**, not a bug.

To diagnose:
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

```bash
MEV_INTRA_DUMP_BLOCK=16817050 MEV_INTRA_DEBUG=1 RUST_LOG=info cargo run -- simulate --block 16817050 --algo dex_dex_intra 2>&1
```
