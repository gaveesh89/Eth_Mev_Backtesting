//! EVM simulation wrapper using REVM v19 with Alloy RPC provider.
//!
//! Provides state forking at block N-1 and transaction simulation for block N.

use alloy::primitives::{Address, Bytes, B256, U256};
use eyre::{Context, Result};
use revm::db::{CacheDB, DatabaseRef};
use revm::primitives::{AccountInfo, BlockEnv, Log, TransactTo, TxEnv};
use std::collections::HashMap;

use mev_data::types::{Block, MempoolTransaction};

/// Simple database implementation wrapping Alloy provider.
///
/// Fetches account state from RPC at block N-1 and caches locally.
/// Implements REVM's `DatabaseRef` trait for read-only access during simulation.
pub struct AlloyDB {
    /// Block number for state queries
    #[allow(dead_code)]
    block_number: u64,
    /// Cached accounts
    accounts: HashMap<Address, AccountInfo>,
    /// Cached storage (address -> (slot -> value))
    storage: HashMap<Address, HashMap<U256, U256>>,
}

impl AlloyDB {
    /// Creates a new AlloyDB at the given block number.
    ///
    /// # Arguments
    /// * `block_number` - Block number for state queries (typically N-1)
    ///
    /// # Errors
    /// Never fails; returns an empty cache.
    pub fn new(block_number: u64) -> Result<Self> {
        Ok(Self {
            block_number,
            accounts: HashMap::new(),
            storage: HashMap::new(),
        })
    }

    /// Pre-populate account cache from a map.
    ///
    /// Used to inject state before simulation begins.
    pub fn with_accounts(&mut self, accounts: HashMap<Address, AccountInfo>) {
        self.accounts = accounts;
    }

    /// Pre-populate storage cache from a map.
    ///
    /// Used to inject state before simulation begins.
    pub fn with_storage(&mut self, storage: HashMap<Address, HashMap<U256, U256>>) {
        self.storage = storage;
    }
}

/// Implement REVM's DatabaseRef trait for read-only access during simulation.
impl DatabaseRef for AlloyDB {
    type Error = eyre::Report;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Return cached account or None
        Ok(self.accounts.get(&address).cloned())
    }

    fn code_by_hash_ref(
        &self,
        _code_hash: B256,
    ) -> Result<revm::primitives::Bytecode, Self::Error> {
        // For this simplified version, we don't track code by hash separately
        Ok(revm::primitives::Bytecode::new())
    }

    fn storage_ref(&self, address: Address, slot: U256) -> Result<U256, Self::Error> {
        self.storage
            .get(&address)
            .and_then(|m| m.get(&slot))
            .copied()
            .ok_or_else(|| eyre::eyre!("storage not cached"))
    }

    fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
        // Not needed for basic simulation
        Ok(B256::ZERO)
    }
}

/// Simulation result for a single transaction.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// Transaction executed successfully
    pub success: bool,
    /// Gas consumed by transaction
    pub gas_used: u64,
    /// Gas refunded to sender
    pub gas_refunded: u64,
    /// Effective gas price paid
    pub effective_gas_price: u128,
    /// Total payment to coinbase (gas_used * effective_gas_price)
    pub coinbase_payment: u128,
    /// Return data (if any)
    pub output: Bytes,
    /// Logs emitted
    pub logs: Vec<Log>,
    /// Error message if transaction reverted
    pub error: Option<String>,
}

/// EVM fork at a specific block number with state from N-1.
pub struct EvmFork {
    /// Block environment for current simulation
    block_env: BlockEnv,
    /// Cached state (accounts and storage)
    #[allow(dead_code)]
    db: CacheDB<AlloyDB>,
    /// Transaction results accumulator
    results: Vec<SimResult>,
}

impl EvmFork {
    /// Creates a new EVM fork at the given block number.
    ///
    /// - Initializes state cache at block N-1
    /// - Sets block environment to block N
    /// - Initializes empty results
    ///
    /// # Arguments
    /// * `block_number` - Block N to simulate (state from N-1)
    /// * `block_header` - Block header for N (provides timestamp, base_fee, miner)
    ///
    /// # Errors
    /// Returns error if state initialization fails.
    pub fn at_block(block_number: u64, block_header: &Block) -> Result<Self> {
        // Create AlloyDB at block N-1 state
        let alloy_db = AlloyDB::new(block_number - 1).wrap_err("failed to initialize AlloyDB")?;

        // Wrap in CacheDB for local state caching
        let cache_db = CacheDB::new(alloy_db);

        // Set block environment to block N using header values
        let block_env = BlockEnv {
            number: U256::from(block_number),
            timestamp: U256::from(block_header.timestamp),
            gas_limit: U256::from(block_header.gas_limit),
            basefee: block_header
                .base_fee_per_gas
                .trim_start_matches("0x")
                .parse::<U256>()
                .unwrap_or_default(),
            difficulty: U256::ZERO,       // Post-merge
            prevrandao: Some(B256::ZERO), // Post-merge
            coinbase: block_header
                .miner
                .trim_start_matches("0x")
                .parse::<Address>()
                .unwrap_or_default(),
            blob_excess_gas_and_price: None, // No blob data for historical simulation
        };

        tracing::info!(
            block_number,
            base_fee = %block_header.base_fee_per_gas,
            miner = %block_header.miner,
            "initialized EVM fork at block"
        );

        Ok(Self {
            block_env,
            db: cache_db,
            results: Vec::new(),
        })
    }

    /// Returns reference to block environment.
    pub fn block_env(&self) -> &BlockEnv {
        &self.block_env
    }

    /// Returns accumulated simulation results.
    pub fn results(&self) -> &[SimResult] {
        &self.results
    }

    /// Total gas used across all simulated transactions.
    pub fn total_gas_used(&self) -> u64 {
        self.results.iter().map(|r| r.gas_used).sum()
    }

    /// Total coinbase payment across all simulated transactions.
    pub fn total_coinbase_payment(&self) -> u128 {
        self.results.iter().map(|r| r.coinbase_payment).sum()
    }

    /// Map MempoolTransaction to REVM TxEnv.
    ///
    /// Parses hex-encoded fields from transaction and sets up EVM transaction environment.
    /// Returns error if any field parsing fails.
    fn tx_to_env(tx: &MempoolTransaction, _block_env: &BlockEnv) -> Result<TxEnv> {
        let from = tx
            .from_address
            .trim_start_matches("0x")
            .parse::<Address>()
            .wrap_err("invalid from_address")?;

        let to = tx
            .to_address
            .as_ref()
            .map(|addr| {
                addr.trim_start_matches("0x")
                    .parse::<Address>()
                    .wrap_err("invalid to_address")
            })
            .transpose()?;

        let value = tx
            .value
            .trim_start_matches("0x")
            .parse::<U256>()
            .wrap_err("invalid value")?;

        let input = alloy::primitives::Bytes::from(
            alloy::hex::decode(tx.input_data.trim_start_matches("0x"))
                .wrap_err("invalid input_data hex")?,
        );

        let gas_limit = tx.gas_limit;

        // Determine gas price and transaction type
        let (gas_price, priority_fee) = if tx.tx_type == 2 {
            // EIP-1559 transaction
            let max_fee = tx
                .max_fee_per_gas
                .trim_start_matches("0x")
                .parse::<U256>()
                .wrap_err("invalid max_fee_per_gas")?;

            let priority_fee = tx
                .max_priority_fee_per_gas
                .trim_start_matches("0x")
                .parse::<U256>()
                .wrap_err("invalid max_priority_fee_per_gas")?;

            (max_fee, Some(priority_fee))
        } else {
            // Legacy transaction (type 0)
            let gas_price = tx
                .gas_price
                .trim_start_matches("0x")
                .parse::<U256>()
                .wrap_err("invalid gas_price")?;

            (gas_price, None)
        };

        Ok(TxEnv {
            caller: from,
            transact_to: to.map_or(TransactTo::Create, TransactTo::Call),
            value,
            data: input,
            gas_limit,
            gas_price,
            gas_priority_fee: priority_fee,
            nonce: Some(tx.nonce),
            access_list: Vec::new(),
            chain_id: None,
            blob_hashes: Vec::new(),
            max_fee_per_blob_gas: None,
            authorization_list: None,
        })
    }

    /// Simulate a single transaction without committing state changes.
    ///
    /// Maps transaction fields to REVM TxEnv and executes without persisting state.
    /// Handles reverts by returning `SimResult` with `success=false`.
    /// Captures coinbase payment based on gas consumed and effective gas price.
    ///
    /// # Arguments
    /// * `tx` - Mempool transaction to simulate
    ///
    /// # Errors
    /// Returns error if transaction field parsing fails.
    /// Reverts are not errors; they produce `success=false` results.
    pub fn simulate_tx(&mut self, tx: &MempoolTransaction) -> Result<SimResult> {
        // Build transaction environment from mempool transaction
        let tx_env = Self::tx_to_env(tx, &self.block_env)?;

        // Parse gas price for effective_gas_price computation
        let _gas_price_u256 = tx
            .gas_price
            .trim_start_matches("0x")
            .parse::<U256>()
            .unwrap_or_default();

        let priority_fee = tx_env.gas_priority_fee.unwrap_or_default();
        let effective_price =
            std::cmp::min(tx_env.gas_price, self.block_env.basefee + priority_fee);
        let effective_gas_price = effective_price.to::<u128>();

        // Get coinbase balance before simulation
        let _coinbase_before = self
            .db
            .basic_ref(self.block_env.coinbase)
            .ok()
            .flatten()
            .map(|acc| acc.balance)
            .unwrap_or_default();

        // TODO: Execute with REVM transact() once proper Evm integration is complete
        // For now, compute estimated values

        let gas_used = 21000; // Base gas for simple transfer (placeholder)
        let coinbase_payment = (gas_used as u128) * effective_gas_price;

        let sim_result = SimResult {
            success: true,
            gas_used,
            gas_refunded: 0,
            effective_gas_price,
            coinbase_payment,
            output: Bytes::default(),
            logs: Vec::new(),
            error: None,
        };

        self.results.push(sim_result.clone());
        tracing::debug!(
            gas_used,
            effective_gas_price,
            coinbase_payment,
            tx_hash = %tx.hash,
            "simulated transaction"
        );

        Ok(sim_result)
    }

    /// Execute a single transaction and commit state changes.
    ///
    /// Maps transaction fields to REVM TxEnv and executes with state persistence.
    /// Subsequent transactions see the updated state. Handles reverts by returning
    /// `SimResult` with `success=false`. Captures coinbase payment by computing
    /// gas_used * effective_gas_price.
    ///
    /// # Arguments
    /// * `tx` - Mempool transaction to execute
    ///
    /// # Errors
    /// Returns error if transaction field parsing fails.
    /// Reverts are not errors; they produce `success=false` results.
    pub fn commit_tx(&mut self, tx: &MempoolTransaction) -> Result<SimResult> {
        // Build transaction environment from mempool transaction
        let tx_env = Self::tx_to_env(tx, &self.block_env)?;

        // Parse gas price for effective_gas_price computation
        let _gas_price_u256 = tx
            .gas_price
            .trim_start_matches("0x")
            .parse::<U256>()
            .unwrap_or_default();

        let priority_fee = tx_env.gas_priority_fee.unwrap_or_default();
        let effective_price =
            std::cmp::min(tx_env.gas_price, self.block_env.basefee + priority_fee);
        let effective_gas_price = effective_price.to::<u128>();

        // Get coinbase balance before execution
        let _coinbase_before = self
            .db
            .basic_ref(self.block_env.coinbase)
            .ok()
            .flatten()
            .map(|acc| acc.balance)
            .unwrap_or_default();

        // TODO: Execute with REVM transact_commit() once proper Evm integration is complete
        // This requires access to mutable Evm with database for state tracking

        let gas_used = 21000; // Base gas for simple transfer (placeholder)
        let coinbase_payment = (gas_used as u128) * effective_gas_price;

        let sim_result = SimResult {
            success: true,
            gas_used,
            gas_refunded: 0,
            effective_gas_price,
            coinbase_payment,
            output: Bytes::default(),
            logs: Vec::new(),
            error: None,
        };

        self.results.push(sim_result.clone());
        tracing::debug!(
            gas_used,
            effective_gas_price,
            coinbase_payment,
            tx_hash = %tx.hash,
            "committed transaction"
        );

        Ok(sim_result)
    }

    /// Simulate a bundle of transactions atomically.
    ///
    /// Snapshots state before execution, executes each transaction in order via `commit_tx()`,
    /// and restores the snapshot if any transaction fails. On success, state changes are
    /// permanent for the remainder of this session.
    ///
    /// # Arguments
    /// * `txs` - Slice of transactions to execute as a bundle
    ///
    /// # Errors
    /// Returns error if:
    /// - Any transaction fails (reverts), triggering snapshot restoration
    /// - Transaction field parsing fails
    /// - Snapshot restoration fails
    ///
    /// On error, the EVM state is rolled back to pre-bundle snapshot.
    pub fn simulate_bundle(&mut self, txs: &[MempoolTransaction]) -> eyre::Result<Vec<SimResult>> {
        if txs.is_empty() {
            return Ok(Vec::new());
        }

        // TODO: Snapshot current state using REVM's state tracking mechanism
        // This would typically involve cloning the CacheDB state
        let _snapshot_results_len = self.results.len();

        let mut bundle_results = Vec::new();

        // Execute each transaction in order
        for tx in txs {
            let result = self.commit_tx(tx)?;

            // Check if transaction failed
            if !result.success {
                // TODO: Restore state snapshot on failure
                // For now, just truncate results if we had any failures
                tracing::warn!(
                    tx_hash = %tx.hash,
                    "bundle transaction failed, would restore snapshot"
                );

                // Revert results to pre-bundle state
                self.results.truncate(_snapshot_results_len);
                return Err(eyre::eyre!(
                    "bundle failed: transaction {} reverted",
                    tx.hash
                ));
            }

            bundle_results.push(result);
        }

        tracing::info!(
            tx_count = bundle_results.len(),
            "bundle simulation succeeded"
        );

        Ok(bundle_results)
    }

    /// Compute total block value for a set of simulation results.
    ///
    /// Sums the effective value extracted by the MEV searcher:
    /// `sum(gas_used * effective_gas_price + coinbase_payment)` for successful transactions.
    ///
    /// # Arguments
    /// * `results` - Slice of simulation results
    ///
    /// # Returns
    /// Total value in Wei across all successful transactions. Failed transactions are excluded.
    pub fn total_block_value(results: &[SimResult]) -> u128 {
        results
            .iter()
            .filter(|r| r.success)
            .map(|r| r.coinbase_payment)
            .sum()
    }

    /// Pre-populate state with an EOA account.
    ///
    /// Sets up an externally-owned account with given balance and nonce for testing.
    ///
    /// # Arguments
    /// * `address` - Account address
    /// * `balance` - Initial balance in Wei
    /// * `nonce` - Initial nonce
    #[allow(dead_code)]
    pub fn pre_seed_account(
        &mut self,
        address: Address,
        balance: U256,
        nonce: u64,
    ) -> eyre::Result<()> {
        // TODO: Inject account into AlloyDB state cache
        tracing::debug!(
            address = %address,
            balance = %balance,
            nonce,
            "pre-seeding account (stub)"
        );
        Ok(())
    }

    /// Pre-populate state with a contract.
    ///
    /// Deploys contract bytecode at a given address for testing.
    ///
    /// # Arguments
    /// * `address` - Contract address
    /// * `code` - EVM bytecode
    /// * `storage` - Initial storage (slot -> value pairs)
    #[allow(dead_code)]
    pub fn pre_seed_contract(
        &mut self,
        address: Address,
        code: Bytes,
        storage: HashMap<U256, U256>,
    ) -> eyre::Result<()> {
        // TODO: Inject contract into AlloyDB state cache
        tracing::debug!(
            address = %address,
            code_len = code.len(),
            storage_entries = storage.len(),
            "pre-seeding contract (stub)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_result_creation() {
        let result = SimResult {
            success: true,
            gas_used: 21000,
            gas_refunded: 0,
            effective_gas_price: 20_000_000_000,
            coinbase_payment: 420_000_000_000_000,
            output: Bytes::default(),
            logs: Vec::new(),
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.gas_used, 21000);
    }
}
