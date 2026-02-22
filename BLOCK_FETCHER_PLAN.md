# BlockFetcher Plan — Alloy RPC Integration

## Overview

Implement `BlockFetcher` in `mev-data/src/blocks.rs` to:
1. Fetch full blocks with all transactions from Alloy provider
2. Fetch transaction receipts (for status and gas_used per tx)
3. Rate-limit concurrent requests (max 10 in-flight)
4. Retry failed requests with exponential backoff
5. Show progress with indicatif progress bars

---

## BlockFetcher Struct Design

```rust
pub struct BlockFetcher {
    // Alloy provider (HTTP or similar)
    provider: Arc<Provider<Http>>,
    
    // Rate limiter: max 10 concurrent requests
    semaphore: Arc<Semaphore>,
    
    // Retry configuration
    max_retries: u32,
    initial_backoff_ms: u64,
    
    // Optional parent store for persisting fetched data
    store: Option<Arc<Store>>,
}
```

---

## Method Signatures & Alloy APIs

### 1. `new(rpc_url: &str, store: Option<Arc<Store>>) -> Result<Self>`

**Purpose:** Initialize provider and rate limiter.

**Implementation Path:**

#### Step A: Parse RPC URL and create HTTP provider
- **Alloy API**: `Http::client()?` to create HTTP transport
- **Alloy API**: `Provider::<Http>::new(http_transport)` to create provider
- Use `ClientBuilder::new()` for connection pooling config (if needed)
- **Error handling**: Wrap in `eyre::Result` with context

#### Step B: Create rate limiter semaphore
- **Tokio API**: `Semaphore::new(10)` — max 10 concurrent
- Wrap in `Arc<Semaphore>` for shared access across tasks

#### Step C: Return initialized BlockFetcher
```rust
pub fn new(rpc_url: &str, store: Option<Arc<Store>>) -> Result<Self> {
    let http = Http::client()?;
    let provider = Provider::<Http>::new(http);
    Ok(Self {
        provider: Arc::new(provider),
        semaphore: Arc::new(Semaphore::new(10)),
        max_retries: 3,
        initial_backoff_ms: 100,
        store,
    })
}
```

---

### 2. `async fn fetch_block_with_txs(&self, block_number: u64) -> Result<(Block, Vec<BlockTransaction>)>`

**Purpose:** Fetch full block + all transactions + receipts in one logical unit.

**Implementation Path:**

#### Step A: Acquire semaphore permit (rate limit)
- **Tokio API**: `self.semaphore.acquire().await?` → `SemaphorePermit`
- Permit auto-releases when dropped → ensures max 10 concurrent
- Wrap in guard or explicit drop after use

#### Step B: Fetch block with full transactions (with retries)
- **Alloy API**: `self.provider.get_block_with_txs(U64::from(block_number)).await`
  - Returns `Option<Block<WithOtherFields<Transaction>>>` (or similar)
  - Full `Block` struct includes:
    - `number` (u64)
    - `hash` (B256)
    - `parent_hash` (B256)
    - `timestamp` (u64)
    - `gas_limit` (u128)
    - `gas_used` (u128)
    - `base_fee_per_gas` (Option<u128>)
    - `miner` (Address)
    - `transactions: Vec<Transaction>` (full txs included)

#### Step C: Convert Block → mev-data::Block struct
- Map Alloy `Block` fields to our schema:
  - `block.number` → `block_number: u64`
  - `block.hash` → `block_hash: String` (encoded as "0x...")
  - `block.parent_hash` → `parent_hash: String`
  - `block.timestamp` → `timestamp: u64`
  - `block.gas_limit` → `gas_limit: u64` (cast from u128)
  - `block.gas_used` → `gas_used: u64` (cast from u128)
  - `block.base_fee_per_gas` → `base_fee_per_gas: String` (Wei as hex)
  - `block.miner` → `miner: String` (Address.to_checksum())
  - `block.transactions.len()` → `transaction_count: u64`

#### Step D: Fetch receipts for each transaction (concurrent)
- For each tx in block.transactions:
  - Call `self.fetch_transaction_receipt(tx.hash, 3)` with retries
  - **Alloy API**: `self.provider.get_transaction_receipt(tx.hash).await`
    - Returns `Option<TransactionReceipt>` with fields:
      - `status`: `Option<U64>` or `Option<bool>` (1=success, 0=revert)
      - `gas_used: u128` (actual gas consumed)
  
- **Concurrency strategy**: 
  - Use `tokio::join_all()` for parallel receipt fetches
  - Each receipt fetch acquires its own permit from semaphore
  - Example: `let receipts = futures::future::join_all(receipt_futures).await;`

#### Step E: Convert Transactions → BlockTransaction structs
- Map each `transaction` + its `receipt` to `BlockTransaction`:
  - `tx.hash` → `tx_hash: String`
  - `tx.from` → `from_address: String`
  - `tx.to` → `to_address: String` (nullable for contract creation)
  - `tx.transaction_index` → `tx_index: u64`
  - `receipt.gas_used` → `gas_used: u64`
  - `receipt.effective_gas_price` or computed → `effective_gas_price: String`
  - `receipt.status` → `status: u32` (1 or 0)

#### Step F: Error handling with retry loop
- Wrap fetch operations in exponential backoff retry:
  ```
  for attempt in 0..max_retries {
      match fetch_operation().await {
          Ok(result) => return Ok(result),
          Err(e) if attempt < max_retries - 1 => {
              sleep(Duration::from_millis(initial_backoff_ms * 2^attempt)).await;
              continue;
          }
          Err(e) => return Err(e).wrap_err("failed after retries"),
      }
  }
  ```

**Signature:**
```rust
#[tracing::instrument(skip(self), fields(block_number))]
pub async fn fetch_block_with_txs(
    &self,
    block_number: u64,
) -> Result<(Block, Vec<BlockTransaction>)> { ... }
```

---

### 3. `async fn fetch_transaction_receipt(&self, tx_hash: B256, retries: u32) -> Result<TransactionReceipt>`

**Purpose:** Fetch receipt for a single tx with retries and rate limiting.

**Implementation Path:**

#### Step A: Acquire semaphore permit
- `let _permit = self.semaphore.acquire().await?;`

#### Step B: Retry loop with exponential backoff
- Loop `0..retries`:
  - **Alloy API**: `self.provider.get_transaction_receipt(tx_hash).await?`
    - Returns `Option<TransactionReceipt>` (None if not found)
  - On success: return `Ok(receipt)`
  - On error + retries remaining: sleep then retry
  - On final error: wrap and return

**Signature:**
```rust
async fn fetch_transaction_receipt(
    &self,
    tx_hash: B256,
    retries: u32,
) -> Result<TransactionReceipt> { ... }
```

---

### 4. `async fn fetch_block_range_with_progress(&self, start: u64, end: u64) -> Result<Vec<(Block, Vec<BlockTransaction>)>>`

**Purpose:** Fetch multiple blocks with progress bar (orchestrator).

**Implementation Path:**

#### Step A: Create progress bar
- **Indicatif API**: `ProgressBar::new((end - start + 1) as u64)`
- Set style with ETA, current count, total
- Draw to stderr (default)

#### Step B: Spawn concurrent fetch tasks
- For each block_number in `start..=end`:
  - Wrap:
    ```
    let fetcher = self.clone_arc(); // Arc<BlockFetcher>
    tokio::spawn(async move {
        let result = fetcher.fetch_block_with_txs(block_number).await;
        progress_bar.inc(1);
        (block_number, result)
    })
    ```
  - Collect all futures in Vec

#### Step C: Await all (with error collection)
- `futures::future::join_all(futures).await`
- Collect both successful (block_number, data) and failed (block_number, error)
- Return errors with context about which blocks failed

#### Step D: Finish progress bar
- `progress_bar.finish_with_message("blocks fetched")`

**Signature:**
```rust
#[tracing::instrument(skip(self), fields(start, end))]
pub async fn fetch_block_range_with_progress(
    &self,
    start: u64,
    end: u64,
) -> Result<Vec<(Block, Vec<BlockTransaction>)>> { ... }
```

---

### 5. `async fn fetch_and_store_range(&self, start: u64, end: u64) -> Result<usize>`

**Purpose:** Fetch blocks + insert into SQLite (requires store to be set).

**Implementation Path:**

#### Step A: Check store is available
```rust
let store = self.store.as_ref().context("store not configured")?;
```

#### Step B: Fetch blocks with progress
```rust
let blocks_data = self.fetch_block_range_with_progress(start, end).await?;
```

#### Step C: Insert into database
- For each `(block, block_txs)`:
  - `store.insert_block(&block)?;`
  - `store.insert_block_txs(&block_txs)?;`
- Total count of rows inserted

**Signature:**
```rust
#[tracing::instrument(skip(self, store), fields(start, end))]
pub async fn fetch_and_store_range(&self, start: u64, end: u64) -> Result<usize> { ... }
```

---

## Alloy Crate API Details

### Key Types & Methods

#### From `alloy_provider::Provider<T>`
- `fn get_block_with_txs(block_id) -> BoxFuture<'_, Option<Block<T>>>` — fetch full block + txs
- `fn get_transaction_receipt(tx_hash) -> BoxFuture<'_, Option<TransactionReceipt>>` — fetch receipt

#### From `alloy_primitives`
- `B256` — 256-bit hash (block hash, tx hash)
- `Address` — 20-byte Ethereum address
- `U64`, `U128` — big integer types for fees, gas

#### From `alloy_rpc_types`
- `Block<T>` — full block with optional fields
  - `Block::WithOtherFields<Transaction>` variant includes full tx data
  - Fields: number, hash, parent_hash, timestamp, gas_limit, gas_used, base_fee_per_gas, miner, transactions
- `Transaction` — transaction struct with from, to, hash, transaction_index, etc.
- `TransactionReceipt` — receipt with status, gas_used, effective_gas_price
- `U128` → use `.to::<u128>()` to convert to primitive

#### From `alloy_transport_http`
- `Http::client()` — create HTTP transport
- Implement connection pooling via `ClientBuilder`

---

## Rate Limiting & Concurrency Strategy

**Semaphore Model:**
```
Acquisition flow for each request:
  1. Task calls semaphore.acquire()
  2. If 10 already active: await queue
  3. Once acquired: permit held for duration of RPC call
  4. On drop: next queued task wakes
  → enforces max 10 concurrent RPC calls
```

**Batching Strategy:**
- Receipts fetched in parallel (one per tx in block)
- But overall backpressure: max 10 concurrent across all operations
- Use `tokio::join_all()` for N-way parallelism within batch

---

## Error Handling & Retry Strategy

**Exponential Backoff:**
```
Attempt 0: immediate
Attempt 1: sleep 100ms * 2^1 = 200ms
Attempt 2: sleep 100ms * 2^2 = 400ms
Attempt 3: sleep 100ms * 2^3 = 800ms
→ gives RPC node time to recover
```

**Error Context:**
```rust
// Example:
Err(e)
  .context("failed to fetch block")?
  .wrap_err_with(|| format!("block_number={}", block_number))?
```

**Alloy provider errors:**
- Network: connection failed, timeout
- RPC: invalid parameters, method not found, internal error
- All wrapped in `eyre::Result` with full context chain

---

## Progress Bar Configuration

**Single block:**
- No progress bar (one-shot call)

**Block range (N blocks):**
```rust
ProgressBar::new(N)
  .with_style(
    ProgressStyle::default_bar()
      .template("{spinner:.green} [{bar:40.cyan}] {pos}/{len} blocks ({eta})")
  )
  .with_message("fetching blocks from RPC")
```

**Multi-block considerations:**
- Each concurrent fetch increments progress by 1 when completed
- Includes both successful and failed attempts in count
- Final message shows: "fetched X blocks" or error count

---

## Module Structure

```rust
// crates/mev-data/src/blocks.rs

//! Alloy RPC provider integration for fetching on-chain block data.
//!
//! Streams blocks with transactions from Ethereum RPC with concurrent
//! rate limiting, retries, and progress tracking.

use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, Http};
use alloy::rpc::types::{Block, Transaction, TransactionReceipt};
use tokio::sync::Semaphore;
use indicatif::ProgressBar;
use eyre::Result;
use std::sync::Arc;

use crate::store::Store;
use crate::types::{Block as MevBlock, BlockTransaction};

pub struct BlockFetcher {
    provider: Arc<Provider<Http>>,
    semaphore: Arc<Semaphore>,
    max_retries: u32,
    initial_backoff_ms: u64,
    store: Option<Arc<Store>>,
}

impl BlockFetcher {
    pub fn new(rpc_url: &str, store: Option<Arc<Store>>) -> Result<Self> { ... }
    
    pub async fn fetch_block_with_txs(&self, block_number: u64) 
        -> Result<(MevBlock, Vec<BlockTransaction>)> { ... }
    
    async fn fetch_transaction_receipt(&self, tx_hash: B256, retries: u32) 
        -> Result<TransactionReceipt> { ... }
    
    pub async fn fetch_block_range_with_progress(&self, start: u64, end: u64)
        -> Result<Vec<(MevBlock, Vec<BlockTransaction>)>> { ... }
    
    pub async fn fetch_and_store_range(&self, start: u64, end: u64)
        -> Result<usize> { ... }
}
```

---

## Integration Points

### With Store (mev-data::store)
- Insert fetched blocks: `store.insert_block(&block)?`
- Insert block transactions: `store.insert_block_txs(&txs)?`

### With Types (mev-data::types)
- Map Alloy `Block` to mev-data `Block`
- Map Alloy `Transaction` + `Receipt` to mev-data `BlockTransaction`

### With CLI (mev-cli)
- Constructor argument: RPC URL from `MEV_RPC_URL` env or clap arg
- Example: `BlockFetcher::new(&rpc_url, Some(Arc::new(store)))?`

---

## Testing Strategy

### Unit Tests (mocked)
- Mock `Provider` using `alloy::mock` or similar
- Test retry logic with controlled errors
- Test semaphore backpressure

### Integration Tests (optional, Phase 1+)
- Use local Reth or Alchemy testnet
- Fetch real block (e.g., Sepolia testnet block 1M)
- Verify struct fields match schema

---

## Performance Targets

| Operation | Target | Notes |
|-----------|--------|-------|
| Single block fetch | < 500ms | Alchemy/Infura latency |
| 50 blocks concurrent | < 15s | With 10 permit limit |
| Receipt fetch per tx | < 100ms avg | Parallelized within batch |
| Backoff sleep | exponential | 100ms→200ms→400ms→800ms |

---

## Next Steps After Implementation

1. Add StoreFetcher wrapper combining Store + BlockFetcher
2. Add `fetch_blocks_for_mempool_txs()` to focus on blocks with pending txs
3. Add CLI command: `mev-cli blocks fetch-range --start 21000000 --end 21001000`
4. Add State fork initialization (Phase 1 mev-sim integration)

---
