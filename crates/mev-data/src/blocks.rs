//! Alloy RPC provider integration for fetching on-chain block data.
//!
//! Streams blocks with transactions from Ethereum RPC with rate limiting,
//! retries, and progress tracking. Uses one receipts call per block.

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::B256;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use eyre::{Context, ContextCompat, Result};
use futures::future::join_all;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::store::Store;
use crate::types::{Block, BlockTransaction};

/// Fetches full blocks with transactions from Ethereum RPC via Alloy provider.
#[derive(Clone)]
pub struct BlockFetcher {
    /// Type-erased provider for RPC calls.
    provider: Arc<DynProvider>,
    /// Global RPC rate limiter.
    semaphore: Arc<Semaphore>,
    /// Max retry attempts for RPC calls.
    max_retries: u32,
    /// Initial backoff in milliseconds.
    initial_backoff_ms: u64,
}

impl BlockFetcher {
    /// Creates a new BlockFetcher and tests RPC connectivity.
    ///
    /// # Errors
    /// Returns error if RPC connection fails or connectivity test fails.
    #[tracing::instrument(skip_all, fields(rpc_url = %rpc_url))]
    pub async fn new(rpc_url: &str) -> Result<Self> {
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect(rpc_url)
            .await?
            .erased();

        let provider = Arc::new(provider);

        let block_number = provider
            .get_block_number()
            .await
            .wrap_err("failed to test RPC connectivity with eth_blockNumber")?;

        tracing::info!(
            rpc_url = %rpc_url,
            latest_block = block_number,
            "RPC connection successful"
        );

        Ok(Self {
            provider,
            semaphore: Arc::new(Semaphore::new(10)),
            max_retries: 3,
            initial_backoff_ms: 100,
        })
    }

    /// Fetches a full block with all transactions and receipts.
    ///
    /// Returns `Ok(None)` if block does not exist.
    ///
    /// # Errors
    /// Returns error if RPC calls fail or required fields are missing.
    #[tracing::instrument(skip(self), fields(block_number))]
    pub async fn fetch_block_with_txs(
        &self,
        block_number: u64,
    ) -> Result<Option<(Block, Vec<BlockTransaction>)>> {
        let block = self
            .retry("fetch block", || async {
                let _permit = self.semaphore.acquire().await.wrap_err("semaphore closed")?;
                self.provider
                    .get_block_by_number(BlockNumberOrTag::Number(block_number))
                    .full()
                    .await
                    .wrap_err("get_block_by_number failed")
            })
            .await?;

        let block = match block {
            Some(block) => block,
            None => {
                tracing::debug!(block_number, "block not found");
                return Ok(None);
            }
        };

        let receipts = self
            .retry("fetch block receipts", || async {
                let _permit = self.semaphore.acquire().await.wrap_err("semaphore closed")?;
                self.provider
                    .get_block_receipts(BlockId::number(block_number))
                    .await
                    .wrap_err("get_block_receipts failed")
            })
            .await?
            .with_context(|| "receipts not found")?;

        let header = &block.header.inner;
        let block_hash = block.header.hash;

        let tx_hashes: Vec<B256> = block.transactions.hashes().collect();
        let mut receipt_map = HashMap::with_capacity(receipts.len());
        for receipt in receipts {
            receipt_map.insert(receipt.transaction_hash, receipt);
        }

        let block = Block {
            block_number: header.number,
            block_hash: format!("{block_hash:#x}"),
            parent_hash: format!("{:#x}", header.parent_hash),
            timestamp: header.timestamp,
            gas_limit: header.gas_limit,
            gas_used: header.gas_used,
            base_fee_per_gas: header
                .base_fee_per_gas
                .map(|fee| format!("0x{fee:x}"))
                .unwrap_or_else(|| "0x0".to_string()),
            miner: format!("{:#x}", header.beneficiary),
            transaction_count: tx_hashes.len() as u64,
        };

        let mut block_txs = Vec::with_capacity(tx_hashes.len());
        for (idx, tx_hash) in tx_hashes.iter().enumerate() {
            let receipt = receipt_map
                .remove(tx_hash)
                .ok_or_else(|| eyre::eyre!("receipt not found for transaction"))?;

            let status = if receipt.status() { 1 } else { 0 };
            let tx_index = receipt.transaction_index.unwrap_or(idx as u64);
            let effective_gas_price = receipt.effective_gas_price;

            block_txs.push(BlockTransaction {
                block_number,
                tx_hash: format!("{tx_hash:#x}"),
                tx_index,
                from_address: format!("{:#x}", receipt.from),
                to_address: receipt
                    .to
                    .map(|addr| format!("{addr:#x}"))
                    .unwrap_or_default(),
                gas_used: receipt.gas_used,
                effective_gas_price: format!("0x{effective_gas_price:x}"),
                status,
            });
        }

        Ok(Some((block, block_txs)))
    }

    /// Fetches a range of blocks with progress tracking.
    ///
    /// # Errors
    /// Returns error if RPC calls or join handles fail.
    #[tracing::instrument(skip(self, store), fields(start, end))]
    pub async fn fetch_range(&self, start: u64, end: u64, store: &Store) -> Result<()> {
        let pb = ProgressBar::new(end.saturating_sub(start) + 1);
        pb.set_style(
            ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan}] {pos}/{len} blocks")
                .wrap_err("failed to configure progress bar")?,
        );

        let mut to_fetch = Vec::new();
        for block_num in start..=end {
            if !store.block_range_exists(block_num, block_num)? {
                to_fetch.push(block_num);
            } else {
                pb.inc(1);
            }
        }

        tracing::info!(
            start,
            end,
            total_blocks = end.saturating_sub(start) + 1,
            blocks_to_fetch = to_fetch.len(),
            "starting block range fetch"
        );

        let fetcher = Arc::new(self.clone());
        let futures = to_fetch.into_iter().map(|block_num| {
            let this = Arc::clone(&fetcher);
            let pb = pb.clone();
            tokio::spawn(async move {
                let result = this.fetch_block_with_txs(block_num).await;
                pb.inc(1);
                (block_num, result)
            })
        });

        let results = join_all(futures).await;
        for join_result in results {
            let (block_num, fetch_result) = join_result.wrap_err("task panicked")?;
            match fetch_result {
                Ok(Some((block, txs))) => {
                    store.insert_block(&block)?;
                    if !txs.is_empty() {
                        store.insert_block_txs(&txs)?;
                    }
                }
                Ok(None) => {
                    tracing::warn!(block_number = block_num, "block not found in RPC");
                }
                Err(err) => {
                    tracing::error!(
                        block_number = block_num,
                        error = %err,
                        "failed to fetch block"
                    );
                }
            }
        }

        pb.finish_with_message("done");
        Ok(())
    }

    async fn retry<F, Fut, T>(&self, context: &'static str, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        for attempt in 0..self.max_retries {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(err) if attempt < self.max_retries - 1 => {
                    let delay = self.initial_backoff_ms * (1u64 << attempt);
                    tracing::warn!(
                        attempt,
                        delay_ms = delay,
                        error = %err,
                        "retrying RPC call"
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(err) => return Err(err).wrap_err(context),
            }
        }

        unreachable!("retry loop should return on last attempt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_requires_valid_url() {
        let result = BlockFetcher::new("invalid://url").await;
        assert!(result.is_err(), "should reject invalid URL");
    }
}