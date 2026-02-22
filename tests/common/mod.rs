//! Shared test helpers and utilities.
//!
//! Provides factory functions for creating test doubles of MEV data structures
//! with sensible defaults.

#![allow(dead_code)]

use mev_data::store::Store;
use mev_data::types::{Block, BlockTransaction, MempoolTransaction};

/// Creates an in-memory SQLite Store for unit tests.
///
/// Uses `:memory:` database with all migrations applied.
/// Suitable for all unit and integration tests that don't require persistence.
///
/// # Panics
/// Panics if the in-memory database cannot be created (should never happen).
///
/// # Example
/// ```ignore
/// let store = test_store();
/// ```
pub fn test_store() -> Store {
    Store::new(":memory:").expect("in-memory store should always open")
}

/// Creates a sample MempoolTransaction with sensible defaults.
///
/// All fields except those provided are set to reasonable test values.
/// Use this factory to quickly construct test transactions without
/// specifying every field.
///
/// # Arguments
/// * `hash` - Transaction hash (hex string without 0x prefix; 0x prefix will be added)
/// * `gas_price` - Gas price in Wei (will be converted to hex text)
///
/// # Example
/// ```ignore
/// let tx = sample_mempool_tx("abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234", 20_000_000_000);
/// assert_eq!(tx.gas_price, "0x4a817c800");
/// ```
pub fn sample_mempool_tx(hash: &str, gas_price: u128) -> MempoolTransaction {
    MempoolTransaction {
        hash: format!("0x{}", hash),
        block_number: None,
        timestamp_ms: 1708617600000, // 2024-02-22T12:00:00Z
        from_address: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266".to_string(),
        to_address: Some("0x70997970c51812e339d9b73b0245ad59e15ebbf9".to_string()),
        value: "0x0".to_string(),
        gas_limit: 21000,
        gas_price: format!("0x{:x}", gas_price),
        max_fee_per_gas: "0x0".to_string(),
        max_priority_fee_per_gas: "0x0".to_string(),
        nonce: 0,
        input_data: "0x".to_string(),
        tx_type: 0,
        raw_tx: "0xf86a8085012a05f20082520894d8da6bf26964af9d7eed9e03e53415d37aa96045880de0b6b3a764000080".to_string(),
    }
}

/// Creates a sample Block with sensible defaults.
///
/// # Arguments
/// * `number` - Block number
///
/// # Example
/// ```ignore
/// let block = sample_block(18_000_000);
/// assert_eq!(block.block_number, 18_000_000);
/// assert_eq!(block.transaction_count, 100);
/// ```
pub fn sample_block(number: u64) -> Block {
    Block {
        block_number: number,
        block_hash: format!("0x{:064x}", number),
        parent_hash: format!("0x{:064x}", number.saturating_sub(1)),
        timestamp: 1708617600 + (number * 12), // ~12 second blocks
        gas_limit: 30_000_000,
        gas_used: 15_000_000,
        base_fee_per_gas: "0x3b9aca00".to_string(), // 1 gwei
        miner: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".to_string(),
        transaction_count: 100,
    }
}

/// Creates a sample BlockTransaction with sensible defaults.
///
/// # Arguments
/// * `block` - Block number for this transaction
/// * `hash` - Transaction hash (hex string without 0x prefix; 0x prefix will be added)
///
/// # Example
/// ```ignore
/// let tx = sample_block_tx(18_000_000, "abcd1234567890abcd1234567890abcd1234567890abcd1234567890abcd1234");
/// assert_eq!(tx.block_number, 18_000_000);
/// ```
pub fn sample_block_tx(block: u64, hash: &str) -> BlockTransaction {
    BlockTransaction {
        block_number: block,
        tx_hash: format!("0x{}", hash),
        tx_index: 42,
        from_address: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266".to_string(),
        to_address: "0x70997970c51812e339d9b73b0245ad59e15ebbf9".to_string(),
        gas_used: 21000,
        effective_gas_price: "0x4a817c800".to_string(), // ~20 gwei
        status: 1,                                      // success
    }
}
