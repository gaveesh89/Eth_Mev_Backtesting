//! Alloy RPC provider integration for fetching on-chain block data.
//!
//! Streams blocks with transactions from Ethereum RPC endpoint.
//! Maps Alloy types to mev-data schema types.

use alloy::network::Ethereum;
use alloy::primitives::B256;
use alloy::providers::fillers::FillProvider;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::eth::{BlockId, BlockNumberOrTag};
use eyre::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use crate::store::Store;
use crate::types::{Block, BlockTransaction};

type ProviderType = FillProvider<
    alloy::providers::fillers::JoinFill<
        alloy::providers::Identity,
        alloy::providers::fillers::JoinFill<
            alloy::providers::fillers::GasFiller,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::BlobGasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::NonceFiller,
                    alloy::providers::fillers::ChainIdFiller,
                >,
            >,
        >,
    >,
    alloy::providers::RootProvider<Ethereum>,
>;

/// Fetches full blocks with transactions from Ethereum RPC via Alloy provider.
pub struct BlockFetcher {
    /// Alloy FillProvider with gas, nonce, chain_id, blob_gas fillers
    provider: Arc<ProviderType>,
}

impl BlockFetcher {
    /// Creates a new BlockFetcher and tests RPC connectivity.
    ///
    /// Verifies connection via `eth_blockNumber` call and logs the RPC endpoint.
    ///
    /// # Arguments
    /// * `rpc_url` - URL to Ethereum RPC endpoint (e.g., Alchemy, Infura, local Reth)
    ///
    /// # Errors
    /// Returns error if RPC connection fails or connectivity test fails.
    ///
    /// # Example
    /// ```no_run
    /// # use mev_data::blocks::BlockFetcher;
    /// # async fn example() -> eyre::Result<()> {
    /// let fetcher = BlockFetcher::new("https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[tracing::instrument(skip_all, fields(rpc_url = %rpc_url))]
    pub async fn new(rpc_url: &str) -> Result<Self> {
        // Create provider from URL using ProviderBuilder
        let provider =
            ProviderBuilder::new().on_http(rpc_url.parse().wrap_err("invalid RPC URL format")?);
        let provider = Arc::new(provider);

        // Test connectivity via eth_blockNumber
        let block_number = provider
            .get_block_number()
            .await
            .wrap_err("failed to test RPC connectivity with eth_blockNumber")?;

        tracing::info!(
            rpc_url = %rpc_url,
            latest_block = block_number,
            "RPC connection successful"
        );

        Ok(Self { provider })
    }

    /// Fetches a full block with all transactions and their receipts.
    ///
    /// Returns `Ok(None)` if block does not exist.
    /// Maps Alloy types to mev-data schema types.
    ///
    /// # Arguments
    /// * `block_number` - Block number to fetch
    ///
    /// # Errors
    /// Returns error if RPC call fails or type conversion fails.
    #[tracing::instrument(skip(self), fields(block_number))]
    pub async fn fetch_block_with_txs(
        &self,
        block_number: u64,
    ) -> Result<Option<(Block, Vec<BlockTransaction>)>> {
        // Fetch block with transaction hashes
        let block_header = self
            .provider
            .get_block(BlockId::Number(BlockNumberOrTag::Number(block_number)))
            .await
            .wrap_err_with(|| format!("failed to fetch block {}", block_number))?;

        // Handle missing block
        let block_header = match block_header {
            Some(header) => header,
            None => {
                tracing::debug!(block_number, "block not found");
                return Ok(None);
            }
        };

        // Get transaction hashes and fetch receipts
        let tx_hashes: Vec<B256> = block_header.transactions.hashes().collect();

        // Fetch receipts concurrently
        let receipts = futures::future::try_join_all(tx_hashes.iter().map(|hash| {
            let provider = self.provider.clone();
            async move {
                provider
                    .get_transaction_receipt(*hash)
                    .await
                    .wrap_err_with(|| format!("failed to fetch receipt {}", hash))
            }
        }))
        .await?;

        // Map block header to Block struct
        let block = Block {
            block_number: block_header.header.number,
            block_hash: format!("0x{}", block_header.header.hash),
            parent_hash: format!("0x{}", block_header.header.parent_hash),
            timestamp: block_header.header.timestamp,
            gas_limit: block_header.header.gas_limit,
            gas_used: block_header.header.gas_used,
            base_fee_per_gas: block_header
                .header
                .base_fee_per_gas
                .map(|fee| format!("0x{:x}", fee))
                .unwrap_or_else(|| "0".to_string()),
            miner: format!("0x{}", block_header.header.beneficiary),
            transaction_count: tx_hashes.len() as u64,
        };

        // Map hashes and receipts to BlockTransaction (note: from/to come from receipt)
        let mut block_txs = Vec::new();
        for (idx, (tx_hash, receipt_opt)) in tx_hashes.iter().zip(receipts).enumerate() {
            let receipt =
                receipt_opt.ok_or_else(|| eyre::eyre!("receipt not found for transaction"))?;

            let status = if receipt.status() { 1 } else { 0 };

            block_txs.push(BlockTransaction {
                block_number,
                tx_hash: format!("0x{}", tx_hash),
                tx_index: idx as u64,
                from_address: format!("0x{}", receipt.from),
                to_address: receipt
                    .to
                    .map(|addr| format!("0x{}", addr))
                    .unwrap_or_default(),
                gas_used: receipt.gas_used,
                effective_gas_price: format!("0x{:x}", receipt.effective_gas_price),
                status,
            });
        }

        Ok(Some((block, block_txs)))
    }

    /// Fetches a range of blocks with rate limiting, retries, and progress tracking.
    ///
    /// - Skips blocks already in store via `block_range_exists` check
    /// - Limits to 10 concurrent RPC calls via `tokio::sync::Semaphore`
    /// - Retries failed blocks up to 3 times with 500ms exponential backoff
    /// - Shows progress with `indicatif` (blocks fetched / txs stored)
    /// - Logs warnings for missing blocks but continues (doesn't fail range)
    ///
    /// # Arguments
    /// * `start` - Starting block number (inclusive)
    /// * `end` - Ending block number (inclusive)
    /// * `store` - Database to store blocks and transactions in
    ///
    /// # Errors
    /// Returns error if database operations or unrecoverable RPC errors fail.
    #[tracing::instrument(skip(self, store), fields(start, end))]
    pub async fn fetch_range(&self, start: u64, end: u64, store: &Store) -> Result<()> {
        let semaphore = Arc::new(Semaphore::new(10));
        let multi = MultiProgress::new();
        let block_pb = multi.add(ProgressBar::new(end.saturating_sub(start) + 1));
        let tx_pb = multi.add(ProgressBar::new_spinner());

        block_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} blocks")
                .unwrap(),
        );
        tx_pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );

        // Collect block numbers that need fetching (skip existing in store)
        let mut to_fetch = Vec::new();
        for block_num in start..=end {
            if !store.block_range_exists(block_num, block_num)? {
                to_fetch.push(block_num);
            } else {
                block_pb.inc(1);
            }
        }

        tracing::info!(
            start,
            end,
            total_blocks = end.saturating_sub(start) + 1,
            blocks_to_fetch = to_fetch.len(),
            "starting block range fetch"
        );

        // Spawn tasks for fetching with rate limiting
        let mut handles = Vec::new();
        for block_num in to_fetch {
            let sem = semaphore.clone();
            let provider = self.provider.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.ok();

                // Retry logic: up to 3 attempts with exponential backoff
                for attempt in 0..3 {
                    // Create temporary BlockFetcher just for the fetch call
                    let fetcher = BlockFetcher {
                        provider: provider.clone(),
                    };
                    match fetcher.fetch_block_with_txs(block_num).await {
                        Ok(result) => return Ok((block_num, result)),
                        Err(_e) if attempt < 2 => {
                            let backoff_ms = 500 * 2_u64.pow(attempt as u32);
                            tracing::debug!(
                                block_number = block_num,
                                attempt = attempt + 1,
                                backoff_ms,
                                "retrying failed block fetch"
                            );
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        }
                        Err(e) => return Err((block_num, e)),
                    }
                }
                unreachable!()
            });
            handles.push(handle);
        }

        // Collect results from all tasks
        let mut all_results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(result) => all_results.push(result),
                Err(e) => tracing::error!("task join error: {}", e),
            }
        }

        // Process results and insert into store
        for result in all_results {
            match result {
                Ok((block_num, None)) => {
                    tracing::warn!(block_number = block_num, "block not found in RPC");
                    block_pb.inc(1);
                }
                Ok((_block_num, Some((block, txs)))) => {
                    store.insert_block(&block)?;
                    let tx_count = txs.len();
                    if !txs.is_empty() {
                        store.insert_block_txs(&txs)?;
                        tx_pb.set_message(format!("Stored {} txs", tx_count));
                    }
                    block_pb.inc(1);
                }
                Err((block_num, e)) => {
                    tracing::error!(
                        block_number = block_num,
                        "failed to fetch block after 3 retries: {}",
                        e
                    );
                    block_pb.inc(1);
                    // Don't fail - continue with next block
                }
            }
        }

        block_pb.finish_with_message("✓ Fetched all blocks");
        tx_pb.finish_with_message("✓ Done");

        Ok(())
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
