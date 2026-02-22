# Tutorial 1: Your First Backtest (Beginner's Guide)

Welcome! This tutorial walks you through a complete MEV backtest in ~15 minutes. We'll fetch real historical data, simulate transaction ordering, and analyze the results.

## Prerequisites

- **Rust 1.82+** installed (`rustup default stable`)
- **RPC endpoint** (free tier at [Alchemy](https://www.alchemy.com/) or [Infura](https://infura.io/))
- **~1GB disk** space
- **Terminal** (bash/zsh)

## Step 0: Get Your RPC URL

1. Sign up for a free account at [Alchemy](https://www.alchemy.com/)
2. Create a new "App" for Ethereum Mainnet
3. Copy the HTTPS URL (looks like `https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY`)
4. Set environment variable:
   ```bash
   export MEV_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
   ```

## Step 1: Clone and Build

```bash
git clone https://github.com/your-org/mev-backtest-toolkit
cd mev-backtest-toolkit
cargo build --release
```

This takes ~2–3 minutes. Grab a coffee! ☕

## Step 2: Fetch Mempool Data for One Day

We'll fetch Flashbots' public mempool data for **2024-01-15**:

```bash
cargo run --release --bin mev-cli -- \
  fetch \
  --date 2024-01-15 \
  --db-path data/mev.db
```

**What's happening?**
- Downloads Flashbots' Parquet file (~50MB) for all transactions seen in mempool that day
- Parses into ~500k structured `MempoolTransaction` records
- Stores in SQLite database at `data/mev.db`
- Takes 30–60 seconds

**Output:**
```
Downloading mempool-dumpster for 2024-01-15...
Downloaded 2024-01-15.parquet (52.3 MB)
Parsing transactions...
Inserted 523,847 transactions into database
```

## Step 3: Fetch On-Chain Block Data

Now let's fetch the actual blocks that were produced. We'll get blocks **18,900,000 through 18,900,010** (10 blocks):

```bash
cargo run --release --bin mev-cli -- \
  fetch \
  --start-block 18900000 \
  --end-block 18900010 \
  --rpc-url "$MEV_RPC_URL" \
  --db-path data/mev.db
```

**What's happening?**
- Connects to your RPC endpoint
- Fetches full block details + all transactions' receipts
- Stores: block header info, transaction list, gas usage, status
- Takes 30–90 seconds (depending on RPC latency)

**Output:**
```
Connecting to RPC: https://eth-mainnet.g.alchemy.com/v2/...
Fetching blocks 18900000–18900010...
[████████████████████] 10/10 blocks fetched
Block 18900000: 149 transactions, 29.98M gas used
Block 18900001: 156 transactions, 29.99M gas used
...
Stored 10 blocks in database
```

## Step 4: Simulate Ordering for One Block

Now comes the MEV analysis! We'll simulate ordering the transactions in block **18900005** using the "effective gas price" (EGP) algorithm:

```bash
cargo run --release --bin mev-cli -- \
  simulate \
  --block 18900005 \
  --algorithm egp \
  --db-path data/mev.db
```

**What's happening?**
- Loads all transactions from the block
- Forks EVM state at block 18900004
- Sorts txs by effective gas price (descending)
- Applies nonce constraints (remove txs with gaps)
- Simulates execution via REVM
- Compares simulated block value vs. actual
- Takes 5–15 seconds

**Output:**
```
Simulating block 18900005 with algorithm: EGP
Loaded 156 transactions
Applied nonce constraints: 3 txs removed (gaps)
Simulated 153 transactions
Total gas used: 29,847,521
Simulated block value: 45.234 ETH
Actual block value: 43.891 ETH
Analysis:
  - Capture rate: 1.0305 (103.05%)
  - MEV captured: 1.343 ETH
  - Private flow: 0.000 ETH
```

## Step 5: Analyze Results

Let's see a summary of the analysis:

```bash
cargo run --release --bin mev-cli -- \
  analyze \
  --block 18900005 \
  --db-path data/mev.db
```

**What's happening?**
- Reads the simulation results from Step 4
- Computes P&L (profit & loss) metrics
- Calculates capture rate (simulated value / actual value)
- Classifies MEV opportunities (sandwich, arbitrage, etc.)

**Output (Table):**
```
┌────────────────────────────────────────────────────────┐
│ Block Analysis: 18900005                               │
├────────────────────────────────────────────────────────┤
│ Block Hash        0x4a5f2c8b9e3d1a7f6c2b9a4d5e6f7c8b   │
│ Miner/Coinbase    0x4838B106FCe9647Bdf1F1C462C429f8d   │
│ Timestamp         2024-01-15 14:22:47 (UTC)            │
│                                                        │
│ Transaction Counts                                     │
│   Total in block  156                                  │
│   After nonce     153                                  │
│   Included        153                                  │
│                                                        │
│ Gas Metrics                                            │
│   Gas limit       30,000,000                           │
│   Gas used        29,847,521 (99.49%)                  │
│   Base fee        25.123 gwei                          │
│                                                        │
│ Block Value (Priority Fees + Tips)                     │
│   Actual          43.891 ETH                           │
│   Simulated (EGP) 45.234 ETH                           │
│   Difference      +1.343 ETH                           │
│                                                        │
│ Capture Rate      1.0305 (103.05%)                     │
│   Interpretation: Optimal EGP ordering would've        │
│                  captured 3.05% more value            │
│                                                        │
│ Classification                                         │
│   Sandwiches      2 detected                           │
│   Arbitrage       5 opportunities                      │
│   Backruns        3 detected                           │
│                                                        │
│ Estimated MEV     1.343 ETH ($4,029 at $3k/ETH)       │
└────────────────────────────────────────────────────────┘
```

## Understanding the Output Table

### Block Identifiers
- **Block Hash**: Unique identifier for this block
- **Miner/Coinbase**: The validator/proposer who built the block
- **Timestamp**: When the block was produced (UTC)

### Transaction Counts
- **Total in block**: Number of txs included on-chain (156)
- **After nonce**: Txs remaining after applying sender nonce constraints. Example: if sender has nonces [1, 3], we remove nonce 3 (skips 2). Result: 153 after removing 3 problematic txs.
- **Included**: How many txs we simulated (should equal "After nonce")

### Gas Metrics
- **Gas limit**: Maximum gas allowed per block (30M post-London)
- **Gas used**: Total gas consumed by all txs (29.847M = 99.49% full)
- **Base fee**: Ethereum's dynamic fee. Higher if block was >50% full. Burned by protocol.

### Block Value
This is the **priority fees** (tips) paid to the proposer.

- **Actual**: What the block actually captured = sum of (effective_gas_price - base_fee) × gas_used for all txs
- **Simulated (EGP)**: What an optimal EGP ordering **would** have captured if all txs were sorted by gas price
- **Difference**: +1.343 ETH means we left money on the table by not perfectly ordering by gas price

### Capture Rate
Ratio = Simulated / Actual = 45.234 / 43.891 = **1.0305** (103.05%)

**Interpretation:**
- 1.0 = perfectly ordered already
- > 1.0 = we could've extracted more value by reordering (missed MEV)
- < 1.0 = we couldn't match actual (rare; suggests private order flow)

**In this case**: The proposer missed ~3% of possible MEV by not sorting perfectly by gas price.

### Classification
MEV opportunities detected (if sandwiches, arbitrage, backruns detected):
- **Sandwiches**: 2 (attacker frontrun + backrun victim tx)
- **Arbitrage**: 5 (buy from pool A, sell to pool B, profit!)
- **Backruns**: 3 (txs that benefited from preceding state changes)

### Estimated MEV
Total MEV value = (Simulated - Actual) = 1.343 ETH

**This is the profit lost to sub-optimal ordering.**

## Step 6: Try Different Algorithms

Want to compare ordering strategies? Try profit-based ordering:

```bash
cargo run --release --bin mev-cli -- \
  simulate \
  --block 18900005 \
  --algorithm profit \
  --db-path data/mev.db
```

**Algorithms:**
- `egp`: Sort by effective gas price (standard Ethereum)
- `profit`: Estimate tx profit, sort by profit (MEV-aware)

You'll notice:
- **EGP ordering**: Captures fees fairly but misses MEV opportunities
- **Profit ordering**: Reorders for maximum value extraction

Compare capture rates to see which would've performed better!

## Step 7: Backtest Multiple Blocks (Pro Move)

Analyze blocks 18900000–18900009:

```bash
for block in {18900000..18900009}; do
  echo "Analyzing block $block..."
  cargo run --release --bin mev-cli -- \
    simulate \
    --block "$block" \
    --algorithm egp \
    --db-path data/mev.db
done
```

Then compute range statistics. You'll see patterns across blocks.

## Troubleshooting

### "failed to connect to RPC"
- Check `$MEV_RPC_URL` is set: `echo $MEV_RPC_URL`
- Verify URL ends with your API key (no `/` at end)
- Test connection: `curl -s "$MEV_RPC_URL" -X POST -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq .`

### "block not found in database"
- Run fetch step for that block first
- Use `status` command to see what blocks are stored: `cargo run --release --bin mev-cli -- status --db-path data/mev.db`

### "too many transactions" / memory error
- Break into smaller block ranges (50 blocks at a time)
- Use `--release` flag to build optimized binary

### Simulation results don't match expectations
- **Reason**: State injection not implemented yet (see `crates/mev-sim/src/evm.rs`)
- **Workaround**: Results are relative; compare algorithms against each other, not against real-world

## What's Next?

- **Master arbitrage math**: Read [Tutorial 2: Arbitrage Math](tutorial-2-arb-math.md)
- **Add custom strategies**: See [CONTRIBUTING.md](../CONTRIBUTING.md)
- **Understand the architecture**: Read [README.md](../README.md) § Architecture
- **Deep dive on concepts**: Check [MEV Glossary](mev-concepts-glossary.md)

## Key Takeaways

✅ MEV exists in every block — transaction ordering matters!  
✅ Simple EGP sorting captures most value but misses MEV opportunities.  
✅ Capture rate shows how much value is being left on the table.  
✅ Sandwiches, arbitrage, and backruns are the main MEV categories.  
✅ This toolkit lets you backtest strategies without live-trading risk.

Happy backtesting! 🚀

