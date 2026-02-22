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
use std::sync::Arc;

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
