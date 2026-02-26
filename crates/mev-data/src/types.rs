//! Type definitions for MEV data structures.

use serde::{Deserialize, Serialize};

/// Mempool transaction data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MempoolTransaction {
    /// Transaction hash (lowercase hex with 0x prefix).
    pub hash: String,
    /// Block number (None if pending).
    pub block_number: Option<u64>,
    /// Timestamp in unix milliseconds.
    pub timestamp_ms: u64,
    /// Sender address (hex text).
    pub from_address: String,
    /// Recipient address (None for contract creation).
    pub to_address: Option<String>,
    /// Transaction value in Wei (stored as hex text).
    pub value: String,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas price in Wei (type 0 txs, stored as hex text).
    pub gas_price: String,
    /// Max fee per gas in Wei (type 2 txs, stored as hex text).
    pub max_fee_per_gas: String,
    /// Max priority fee per gas in Wei (type 2 txs, stored as hex text).
    pub max_priority_fee_per_gas: String,
    /// Nonce.
    pub nonce: u64,
    /// Input data (hex encoded).
    pub input_data: String,
    /// Transaction type (0=legacy, 2=EIP1559).
    pub tx_type: u32,
    /// Raw transaction (hex-encoded RLP).
    pub raw_tx: String,
}

/// On-chain block data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// Block number.
    pub block_number: u64,
    /// Block hash (hex text).
    pub block_hash: String,
    /// Parent block hash (hex text).
    pub parent_hash: String,
    /// Timestamp in unix seconds.
    pub timestamp: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas used.
    pub gas_used: u64,
    /// Base fee per gas in Wei (stored as hex text).
    pub base_fee_per_gas: String,
    /// Miner/coinbase address (hex text).
    pub miner: String,
    /// Number of transactions in block.
    pub transaction_count: u64,
}

/// Transaction included in a block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockTransaction {
    /// Block number.
    pub block_number: u64,
    /// Transaction hash (hex text).
    pub tx_hash: String,
    /// Transaction index in block.
    pub tx_index: u64,
    /// Sender address (hex text).
    pub from_address: String,
    /// Recipient address (hex text).
    pub to_address: String,
    /// Gas used.
    pub gas_used: u64,
    /// Effective gas price in Wei (stored as hex text).
    pub effective_gas_price: String,
    /// Execution status (1=success, 0=revert).
    pub status: u32,
}

/// Transaction receipt log entry stored in SQLite.
///
/// Stores every event log emitted by transactions so that downstream
/// crates can build transfer graphs and detect MEV via SCC analysis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TxLog {
    /// Block number containing this log.
    pub block_number: u64,
    /// Transaction hash that emitted this log (lowercase hex with 0x).
    pub tx_hash: String,
    /// Transaction index within the block.
    pub tx_index: u64,
    /// Log index within the block (global ordering).
    pub log_index: u64,
    /// Address of the contract that emitted the log (hex with 0x).
    pub address: String,
    /// Event signature topic (topic0, hex with 0x).
    pub topic0: String,
    /// First indexed parameter (hex with 0x), if present.
    pub topic1: Option<String>,
    /// Second indexed parameter (hex with 0x), if present.
    pub topic2: Option<String>,
    /// Third indexed parameter (hex with 0x), if present.
    pub topic3: Option<String>,
    /// Non-indexed log data (hex with 0x prefix).
    pub data: String,
}
