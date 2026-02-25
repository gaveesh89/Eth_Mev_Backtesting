# BlockFetcher Plan (Corrected) - Alloy RPC Integration

## Overview

Implement BlockFetcher in `crates/mev-data/src/blocks.rs` to:
1. Fetch full blocks with transactions via Alloy ProviderBuilder
2. Fetch receipts in one call per block (`eth_getBlockReceipts`)
3. Rate-limit RPC calls with a single semaphore (leaf-level acquisition)
4. Retry failed RPC calls with exponential backoff
5. Show progress with indicatif

This plan is the minimum implementation path to see the project in action.

---

## Correct Alloy Construction

Alloy uses ProviderBuilder, not `Provider::<Http>::new`.

```rust
use alloy::providers::{DynProvider, ProviderBuilder};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub struct BlockFetcher {
    provider: Arc<DynProvider>,
    semaphore: Arc<Semaphore>,
    max_retries: u32,
    initial_backoff_ms: u64,
    store: Option<Arc<Store>>,
}

impl BlockFetcher {
    pub fn new(rpc_url: &str, store: Option<Arc<Store>>) -> eyre::Result<Self> {
        let url = rpc_url.parse().wrap_err("invalid RPC URL")?;
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_http(url)
            .erased();

        Ok(Self {
            provider: Arc::new(provider),
            semaphore: Arc::new(Semaphore::new(10)),
            max_retries: 3,
            initial_backoff_ms: 100,
            store,
        })
    }
}
```

---

## Fetch a Single Block (1 block call + 1 receipts call)

Use `get_block_by_number(...).full()` and `get_block_receipts(...)`.

```rust
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::rpc::types::Block;

#[tracing::instrument(skip(self), fields(block_number))]
pub async fn fetch_block_with_txs(
    &self,
    block_number: u64,
) -> eyre::Result<(MevBlock, Vec<BlockTransaction>)> {
    let _permit = self.semaphore.acquire().await?;

    let block = self
        .retry(|| async {
            self.provider
                .get_block_by_number(BlockNumberOrTag::Number(block_number))
                .full()
                .await
        })
        .await?
        .context("block not found")?;

    let receipts = self
        .retry(|| async {
            self.provider
                .get_block_receipts(BlockId::number(block_number))
                .await
        })
        .await?
        .context("receipts not found")?;

    let mev_block = map_block(&block)?;
    let txs = map_block_transactions(&block, &receipts)?;

    Ok((mev_block, txs))
}
```

---

## Correct Block Field Access

Alloy block fields are in `block.header.inner` (consensus header):

```rust
let header = &block.header.inner;
let number = header.number;
let hash = block.header.hash;
let parent_hash = header.parent_hash;
let timestamp = header.timestamp;
let gas_limit = header.gas_limit; // u64
let gas_used = header.gas_used;   // u64
let base_fee = header.base_fee_per_gas; // Option<u64>
let beneficiary = header.beneficiary;  // Address
```

Receipt gas_used is u64 and status is accessed via trait:

```rust
let gas_used = receipt.gas_used; // u64
let status = receipt.inner.status(); // bool
```

---

## Retry Helper (Correct Exponentiation)

```rust
async fn retry<F, Fut, T>(&self, f: F) -> eyre::Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, RpcError>>, // use your concrete error type
{
    for attempt in 0..self.max_retries {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) if attempt < self.max_retries - 1 => {
                let delay = self.initial_backoff_ms * (1u64 << attempt);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                tracing::warn!(attempt, delay_ms = delay, err = %err, "retrying RPC call");
            }
            Err(err) => return Err(err).wrap_err("failed after retries"),
        }
    }
    unreachable!()
}
```

---

## Fetch Range With Progress

Receiver should be `self: &Arc<Self>` (no clone_arc helper).

```rust
pub async fn fetch_block_range_with_progress(
    self: &Arc<Self>,
    start: u64,
    end: u64,
) -> eyre::Result<Vec<(MevBlock, Vec<BlockTransaction>)>> {
    let pb = ProgressBar::new(end - start + 1);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan}] {pos}/{len} blocks ({eta})")?
    );

    let futures: Vec<_> = (start..=end)
        .map(|num| {
            let this = Arc::clone(self);
            let pb = pb.clone();
            tokio::spawn(async move {
                let result = this.fetch_block_with_txs(num).await;
                pb.inc(1);
                (num, result)
            })
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    pb.finish_with_message("done");

    let mut blocks = Vec::with_capacity(results.len());
    for join_result in results {
        let (num, fetch_result) = join_result.wrap_err("task panicked")?;
        blocks.push(fetch_result.wrap_err_with(|| format!("block {num}"))?);
    }

    Ok(blocks)
}
```

---

## Store Persistence

```rust
pub async fn fetch_and_store_range(
    self: &Arc<Self>,
    start: u64,
    end: u64,
) -> eyre::Result<usize> {
    let store = self.store.as_ref().context("store not configured")?;
    let blocks_data = self.fetch_block_range_with_progress(start, end).await?;

    let mut rows = 0usize;
    for (block, txs) in &blocks_data {
        store.insert_block(block)?;
        store.insert_block_txs(txs)?;
        rows += 1 + txs.len();
    }

    Ok(rows)
}
```

---

## Execution Plan (See Project In Action)

1. Implement BlockFetcher in `crates/mev-data/src/blocks.rs` using this plan.
2. Wire a CLI command, for example:

```bash
cargo run -p mev-cli -- blocks fetch-range --start 21000000 --end 21000050
```

3. Observe progress bar and SQLite inserts for blocks and receipts.
4. Use the stored data to drive simulation and analysis.

---

## Notes

- Use `get_block_receipts` to avoid per-transaction RPC calls.
- Apply the semaphore only at the RPC call level to avoid deadlocks.
- Use `DynProvider` to avoid concrete provider types in the struct.
- All gas fields are u64 in Alloy block header and receipts.
