# Intra-Block Scanning

## Architecture

Detects cross-DEX arbitrage opportunities created mid-block after prior transactions shift pool reserves.

### Merged Timeline Algorithm

1. Resolve all tracked pool addresses (Uniswap V2 + SushiSwap) for each pair config.
2. Fetch Sync logs from `eth_getLogs` for the target block, filtered to tracked pool addresses.
3. Decode all Sync events into `ReserveSnapshot` structs with `(tx_index, log_index, reserve0, reserve1)`.
4. Sort snapshots by `(tx_index, log_index)` into a single merged timeline.
5. Initialize latest reserves from block N-1 (pre-state).
6. Walk the timeline: after each snapshot, re-evaluate only the pairs referencing the updated pool.
7. Use `detect_v2_arb_opportunity_with_reason` for full profitability check + reject reason.
8. Emit candidate-trigger rows when spread >= 30 bps even if sizing solver rejects.

### Pool-to-Pair Index

A `HashMap<Address, Vec<usize>>` maps each pool address to the pair indices that reference it. This avoids O(pairs) iteration per snapshot — only affected pairs are re-evaluated.

## Integer Math

- **Spread**: computed via `spread_bps_integer()` using U256 cross-multiplication (no floats).
- **`IntraBlockArb.spread_bps`**: `u128` (was `f64`).
- **Profit**: from `detect_v2_arb_opportunity_with_reason`, which uses integer AMM math throughout.

## Pair Universe

Default pairs (WETH-based):
- WETH/USDC (Uniswap V2 ↔ SushiSwap)
- WETH/USDT (Uniswap V2 ↔ SushiSwap)
- WETH/DAI (Uniswap V2 ↔ SushiSwap)

Stable-stable pairs (zero gas basis):
- USDC/USDT
- USDC/DAI
- USDT/DAI

## Instrumentation

| Env Var | Effect |
|---------|--------|
| `MEV_INTRA_DEBUG=1` | Per-block summary: sync counts, max spread, candidate/profitable counts |
| `MEV_INTRA_DUMP_BLOCK=<N>` | Dumps first 30 timeline steps for block N with reserves, spread, verdict |

## Storage Schema

```sql
CREATE TABLE intra_block_arbs (
    block_number INTEGER NOT NULL,
    after_tx_index INTEGER NOT NULL,
    after_log_index INTEGER NOT NULL,
    pool_a TEXT NOT NULL,
    pool_b TEXT NOT NULL,
    spread_bps INTEGER NOT NULL,
    profit_wei TEXT NOT NULL,
    direction TEXT NOT NULL,
    verdict TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (block_number, after_log_index)
);
```

Rows with `profit_wei = '0x0'` and non-empty `verdict` are candidate-trigger rows — spread was significant but the solver rejected the opportunity.

## Usage

```bash
# Basic scan
cargo run -- simulate --block 16817050 --algo dex_dex_intra

# With debug output
MEV_INTRA_DEBUG=1 cargo run -- simulate --block 16817050 --algo dex_dex_intra

# Dump timeline for debugging
MEV_INTRA_DUMP_BLOCK=16817050 RUST_LOG=info cargo run -- simulate --block 16817050 --algo dex_dex_intra
```
