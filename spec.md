# spec.md — MEV Backtest Toolkit Architecture Reference

> This is a reference document, not an execution guide.
> Agents: read relevant sections when context is needed. Do not read the whole file every turn.

---

## Data Flow

```
mempool-dumpster Parquet files
        ↓ (mev-data::mempool)
   MempoolTransaction structs
        ↓ (mev-data::store)
      SQLite database
        ↑ also written by
   On-chain block data ← Alloy RPC provider (mev-data::blocks)
        ↓
   mev-sim: REVM state fork at block N-1
        ↓
   Transaction ordering (EGP sort or profit sort)
        ↓
   MEV strategy detection (arbitrage, backrun, sandwich)
        ↓
   SimResult + MevOpportunity structs
        ↓ (mev-data::store)
      SQLite database
        ↓
   mev-analysis: P&L computation, classification
        ↓
   mev-cli: table / JSON / CSV output
```

---

## SQLite Schema

### mempool_transactions
| Column | Type | Notes |
|--------|------|-------|
| hash | TEXT PK | lowercase hex with 0x |
| block_number | INTEGER | NULL if pending |
| timestamp_ms | INTEGER | unix ms when first seen |
| from_address | TEXT | |
| to_address | TEXT | nullable (contract creation) |
| value | TEXT | Wei as hex string |
| gas_limit | INTEGER | |
| gas_price | TEXT | Wei as hex (type 0 txs) |
| max_fee_per_gas | TEXT | Wei as hex (type 2 txs) |
| max_priority_fee_per_gas | TEXT | Wei as hex (type 2 txs) |
| nonce | INTEGER | |
| input_data | TEXT | hex encoded |
| tx_type | INTEGER | 0=legacy, 2=EIP1559 |
| raw_tx | TEXT | hex-encoded RLP |

### blocks
| Column | Type | Notes |
|--------|------|-------|
| block_number | INTEGER PK | |
| block_hash | TEXT | |
| parent_hash | TEXT | |
| timestamp | INTEGER | unix seconds |
| gas_limit | INTEGER | |
| gas_used | INTEGER | |
| base_fee_per_gas | TEXT | Wei as hex |
| miner | TEXT | coinbase address |
| transaction_count | INTEGER | |

### block_transactions
| Column | Type | Notes |
|--------|------|-------|
| block_number | INTEGER FK | |
| tx_hash | TEXT | |
| tx_index | INTEGER | |
| from_address | TEXT | |
| to_address | TEXT | |
| gas_used | INTEGER | |
| effective_gas_price | TEXT | Wei as hex |
| status | INTEGER | 1=success, 0=revert |
PK: (block_number, tx_hash)

### simulation_results
| Column | Type | Notes |
|--------|------|-------|
| id | INTEGER PK AUTOINCREMENT | |
| block_number | INTEGER | |
| ordering_algorithm | TEXT | 'egp' or 'profit' |
| simulated_at | TEXT | ISO8601 |
| tx_count | INTEGER | |
| gas_used | INTEGER | |
| total_value_wei | TEXT | Wei as hex |
| mev_captured_wei | TEXT | Wei as hex |

### mev_opportunities
| Column | Type | Notes |
|--------|------|-------|
| id | INTEGER PK AUTOINCREMENT | |
| simulation_id | INTEGER FK | |
| opportunity_type | TEXT | 'arbitrage','backrun','sandwich' |
| profit_wei | TEXT | Wei as hex |
| tx_hashes | TEXT | JSON array |
| protocol | TEXT | e.g. 'uniswap_v2' |
| details | TEXT | JSON blob |

---

## Key External Addresses (Ethereum Mainnet)

```rust
// Uniswap V2
pub const UNISWAP_V2_ROUTER: &str    = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";
pub const UNISWAP_V2_FACTORY: &str   = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f";

// Uniswap V3
pub const UNISWAP_V3_ROUTER: &str    = "0xE592427A0AEce92De3Edee1F18E0157C05861564";
pub const UNISWAP_V3_FACTORY: &str   = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

// Tokens
pub const WETH: &str   = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
pub const USDC: &str   = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
pub const USDT: &str   = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub const DAI: &str    = "0x6B175474E89094C44Da98b954EedeAC495271d0F";
pub const WBTC: &str   = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599";
```

---

## Uniswap V2 Arbitrage Math

For two pools with reserves (r0_a, r1_a) and (r0_b, r1_b) trading token0/token1:

```
Price discrepancy exists if: r1_a/r0_a ≠ r1_b/r0_b

Optimal input amount (constant-product derivation):
  optimal_in = sqrt(r0_a * r0_b * r1_a / r1_b) - r0_a

After-fee swap output (0.3% fee = fee_num=997, fee_den=1000):
  amount_out = (amount_in * fee_num * reserve_out)
               / (reserve_in * fee_den + amount_in * fee_num)

Profit = output_of_second_swap - optimal_in (in token0 terms)
```

---

## MEV Strategy Classification Heuristics

### Arbitrage
- Two or more DEX swaps in same tx
- Net token flow returns to sender with profit
- Confidence: HIGH if profit > gas cost, MEDIUM otherwise

### Sandwich
- Three consecutive txs: tx[0] and tx[2] same sender, tx[1] different sender
- tx[0] buys token X, tx[1] swaps token X, tx[2] sells token X  
- tx[0] and tx[2] have higher gas priority than tx[1]
- Confidence: HIGH if all three conditions met

### Backrun
- Immediately follows a large swap (>1% pool price impact)
- Same token pair, opposite direction
- Confidence: MEDIUM (heuristic, may miss multi-hop)

---

## Dependency Versions (pinned — do not change)

```toml
alloy = "0.12"
revm = "19"
arrow = "53"
parquet = "53"
rusqlite = "0.32"
tokio = "1"
clap = "4"
serde = "1"
eyre = "0.6"
color-eyre = "0.6"
tracing = "0.1"
tracing-subscriber = "0.3"
reqwest = "0.12"
chrono = "1"
comfy-table = "7"
indicatif = "0.17"
criterion = "0.5"
dashmap = "6"
```

---

## RPC Rate Limit Reference

| Provider | Free tier | Recommendation |
|----------|-----------|---------------|
| Alchemy | ~330 req/min | Good for <50 blocks |
| Infura | ~100 req/min | OK for demos |
| Local Reth | Unlimited | Required for 100+ blocks |

Set via environment variable: `MEV_RPC_URL=https://...`
