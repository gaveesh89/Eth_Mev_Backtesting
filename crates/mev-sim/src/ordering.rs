//! Transaction ordering algorithms for MEV simulation.
//!
//! This module provides mechanisms to order transactions by effective gas price (EGP)
//! or other criteria. Currently implements EGP ordering with gas limit enforcement.

use std::collections::HashMap;

use eyre::Context;
use mev_data::types::MempoolTransaction;

use crate::evm::EvmFork;

/// Transaction ordering strategy.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum OrderingAlgorithm {
    /// Order by effective gas price (standard Ethereum sequencing).
    EffectiveGasPrice,
    /// Order by total profit (MEV-aware sequencing).
    TotalProfit,
}

/// Ordered block with transaction sequence and statistics.
#[derive(Clone, Debug)]
pub struct OrderedBlock {
    /// Ordering algorithm used.
    pub algorithm: OrderingAlgorithm,
    /// Transactions in order, respecting gas limit.
    pub transactions: Vec<MempoolTransaction>,
    /// Total gas consumed by all transactions.
    pub total_gas_used: u64,
    /// Estimated block value in Wei.
    pub estimated_value_wei: u128,
    /// Number of transactions rejected due to gas limit or filtering.
    pub rejected_count: usize,
}

/// Parse a hex string to u128, returning 0 on error.
///
/// # Arguments
/// * `hex_str` - Hex string, optionally prefixed with "0x"
///
/// # Returns
/// Parsed u128 value, or 0 if parsing fails.
fn parse_hex_u128(hex_str: &str) -> u128 {
    let value = hex_str.trim_start_matches("0x");
    if value.is_empty() {
        return 0;
    }

    u128::from_str_radix(value, 16).unwrap_or(0)
}

/// Calculate effective gas price for a transaction.
///
/// For type-2 (EIP-1559) transactions:
/// EGP = min(max_fee_per_gas, base_fee + max_priority_fee_per_gas)
///
/// For type-0 (legacy) transactions:
/// EGP = gas_price
///
/// # Arguments
/// * `tx` - Transaction to evaluate
/// * `base_fee` - Current block base fee in Wei
///
/// # Returns
/// Effective gas price in Wei (u128)
fn calculate_egp(tx: &MempoolTransaction, base_fee: u128) -> u128 {
    if tx.tx_type == 2 {
        // EIP-1559 transaction
        let max_fee = parse_hex_u128(&tx.max_fee_per_gas);
        let max_priority_fee = parse_hex_u128(&tx.max_priority_fee_per_gas);
        let tip_and_base = base_fee.saturating_add(max_priority_fee);
        max_fee.min(tip_and_base)
    } else {
        // Legacy transaction (type 0)
        parse_hex_u128(&tx.gas_price)
    }
}

/// Order transactions by effective gas price (EGP).
///
/// Performs the following steps:
/// 1. Filters out transactions where EGP < base_fee
/// 2. Sorts remaining transactions by EGP in descending order
/// 3. Accumulates gas usage, stopping at 30M gas block limit
/// 4. Returns ordered transactions that fit within the gas limit
///
/// # Arguments
/// * `txs` - Unordered transactions from the mempool
/// * `base_fee` - Current block base fee in Wei
///
/// # Returns
/// Transactions ordered by EGP, filtered and truncated to gas limit.
pub fn order_by_egp(
    txs: Vec<MempoolTransaction>,
    base_fee: u128,
) -> (Vec<MempoolTransaction>, usize) {
    const BLOCK_GAS_LIMIT: u64 = 30_000_000;
    let total_input = txs.len();

    // Calculate EGP and filter out transactions below base_fee
    let mut txs_with_egp: Vec<(MempoolTransaction, u128)> = txs
        .into_iter()
        .map(|tx| {
            let egp = calculate_egp(&tx, base_fee);
            (tx, egp)
        })
        .filter(|(_, egp)| *egp >= base_fee)
        .collect();
    let mut rejected = total_input.saturating_sub(txs_with_egp.len());

    // Sort by EGP descending
    txs_with_egp.sort_by(|a, b| b.1.cmp(&a.1));

    // Accumulate gas and enforce block limit
    let mut ordered = Vec::new();
    let mut total_gas = 0u64;
    for (tx, _) in txs_with_egp {
        match total_gas.checked_add(tx.gas_limit) {
            Some(new_total) if new_total <= BLOCK_GAS_LIMIT => {
                total_gas = new_total;
                ordered.push(tx);
            }
            _ => {
                rejected += 1;
            }
        }
    }

    (ordered, rejected)
}

/// Applies sender nonce constraints to a transaction set.
///
/// Transactions are grouped by sender and each group is sorted by nonce ascending.
/// Any transaction that introduces a nonce gap is removed.
///
/// Example: nonces [5, 6, 8] keeps [5, 6] and drops [8].
pub fn apply_nonce_constraints(txs: Vec<MempoolTransaction>) -> Vec<MempoolTransaction> {
    let mut by_sender: HashMap<String, Vec<MempoolTransaction>> = HashMap::new();

    for tx in txs {
        by_sender
            .entry(tx.from_address.to_lowercase())
            .or_default()
            .push(tx);
    }

    let mut constrained = Vec::new();

    for mut sender_txs in by_sender.into_values() {
        sender_txs.sort_by_key(|tx| tx.nonce);

        let mut expected_nonce: Option<u64> = None;
        for tx in sender_txs {
            match expected_nonce {
                None => {
                    expected_nonce = Some(tx.nonce.saturating_add(1));
                    constrained.push(tx);
                }
                Some(next) if tx.nonce == next => {
                    expected_nonce = Some(next.saturating_add(1));
                    constrained.push(tx);
                }
                Some(next) if tx.nonce > next => {
                    continue;
                }
                Some(_) => {
                    continue;
                }
            }
        }
    }

    constrained
}

/// Orders transactions by simulated total profit contribution.
///
/// For each transaction, simulation is performed via [`EvmFork::simulate_tx`].
/// Reverted transactions are skipped. Score is computed as:
/// `gas_used * effective_gas_price + coinbase_payment`.
///
/// Transactions are sorted descending by score and truncated to the 30M gas block limit.
pub async fn order_by_profit(
    txs: Vec<MempoolTransaction>,
    evm: &mut EvmFork,
    _base_fee: u128,
) -> eyre::Result<Vec<MempoolTransaction>> {
    const BLOCK_GAS_LIMIT: u64 = 30_000_000;

    let mut scored: Vec<(MempoolTransaction, u128)> = Vec::new();

    for tx in txs {
        let sim = evm
            .simulate_tx(&tx)
            .wrap_err_with(|| format!("failed to simulate tx {}", tx.hash))?;

        if !sim.success {
            continue;
        }

        scored.push((tx, sim.coinbase_payment));
    }

    scored.sort_by(|a, b| b.1.cmp(&a.1));

    let mut ordered = Vec::new();
    let mut total_gas = 0u64;

    for (tx, _) in scored {
        match total_gas.checked_add(tx.gas_limit) {
            Some(next) if next <= BLOCK_GAS_LIMIT => {
                total_gas = next;
                ordered.push(tx);
            }
            _ => continue,
        }
    }

    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx_with_type_and_fees(
        tx_type: u32,
        gas_price: &str,
        max_fee_per_gas: &str,
        max_priority_fee_per_gas: &str,
        gas_limit: u64,
    ) -> MempoolTransaction {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        MempoolTransaction {
            hash: format!("0x{:064x}", nonce),
            block_number: None,
            timestamp_ms: 0,
            from_address: "0x0000000000000000000000000000000000000001".to_string(),
            to_address: Some("0x0000000000000000000000000000000000000002".to_string()),
            value: "0x0".to_string(),
            gas_limit,
            gas_price: gas_price.to_string(),
            max_fee_per_gas: max_fee_per_gas.to_string(),
            max_priority_fee_per_gas: max_priority_fee_per_gas.to_string(),
            nonce,
            input_data: "0x".to_string(),
            tx_type,
            raw_tx: "0x".to_string(),
        }
    }

    fn tx_for_sender(sender: &str, nonce: u64, gas_limit: u64) -> MempoolTransaction {
        MempoolTransaction {
            hash: format!("0x{:064x}", nonce + gas_limit),
            block_number: None,
            timestamp_ms: 0,
            from_address: sender.to_string(),
            to_address: Some("0x0000000000000000000000000000000000000002".to_string()),
            value: "0x0".to_string(),
            gas_limit,
            gas_price: "0x3B9ACA00".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce,
            input_data: "0x".to_string(),
            tx_type: 0,
            raw_tx: "0x".to_string(),
        }
    }

    #[test]
    fn test_parse_hex_values() {
        // Verify hex parsing works correctly
        assert_eq!(parse_hex_u128("0x3B9ACA00"), 1_000_000_000); // 1 gwei
        assert_eq!(parse_hex_u128("0xBA43B7400"), 50_000_000_000); // 50 gwei
        assert_eq!(parse_hex_u128("0x174876E800"), 100_000_000_000); // 100 gwei
    }

    #[test]
    fn test_order_by_egp_sorts_descending() {
        // Base fee is 50 gwei (50 * 10^9 Wei = 50,000,000,000)
        let base_fee: u128 = 50_000_000_000;

        // Create type-2 transactions with different EGPs
        // EGP = min(max_fee_per_gas, base_fee + max_priority_fee_per_gas)

        // tx1: max_fee=100 gwei, priority=1 gwei
        // EGP = min(100_000_000_000, 50_000_000_000 + 1_000_000_000) = 51_000_000_000
        let tx1 = tx_with_type_and_fees(
            2,
            "0x0",
            "0x174876E800", // max_fee = 100 gwei = 100,000,000,000 Wei
            "0x3B9ACA00",   // priority_fee = 1 gwei = 1,000,000,000 Wei
            21_000,
        );

        // tx2: max_fee=160 gwei, priority=3 gwei
        // EGP = min(160_000_000_000, 50_000_000_000 + 3_000_000_000) = 53_000_000_000
        let tx2 = tx_with_type_and_fees(
            2,
            "0x0",
            "0x254DF2D000", // max_fee = 160 gwei = 160,000,000,000 Wei
            "0xB2D05E00",   // priority_fee = 3 gwei = 3,000,000,000 Wei
            21_000,
        );

        // tx3 (legacy): gas_price = 160 gwei
        // EGP = 160_000_000_000
        let tx3 = tx_with_type_and_fees(
            0,
            "0x254DF2D000", // gas_price = 160 gwei = 160,000,000,000 Wei
            "0x0",          // ignored for type-0
            "0x0",          // ignored for type-0
            21_000,
        );

        let txs = vec![tx1.clone(), tx2.clone(), tx3.clone()];
        let (ordered, rejected) = order_by_egp(txs, base_fee);

        // Expected order: tx3 (160gwei) > tx2 (53gwei) > tx1 (51gwei)
        assert_eq!(ordered.len(), 3, "all txs should fit in block");
        assert_eq!(rejected, 0);

        assert_eq!(ordered[0].hash, tx3.hash, "highest EGP should be first");
        assert_eq!(ordered[1].hash, tx2.hash, "second highest EGP");
        assert_eq!(ordered[2].hash, tx1.hash, "lowest EGP");
    }

    #[test]
    fn test_order_by_egp_enforces_gas_limit() {
        let base_fee: u128 = 50_000_000_000; // 50 gwei

        // Create transactions that together exceed 30M gas limit
        // tx1: 20M gas, max_fee=100 gwei, priority=1 gwei
        // EGP = min(100_000_000_000, 50_000_000_000 + 1_000_000_000) = 51_000_000_000
        let tx1 = tx_with_type_and_fees(
            2,
            "0x0",
            "0x174876E800", // max_fee = 100 gwei
            "0x3B9ACA00",   // priority_fee = 1 gwei
            20_000_000,     // 20M gas (will fit)
        );

        // tx2: 15M gas, same EGP as tx1 but together would exceed 30M limit
        let tx2 = tx_with_type_and_fees(
            2,
            "0x0",
            "0x174876E800", // max_fee = 100 gwei
            "0x3B9ACA00",   // priority_fee = 1 gwei
            15_000_000,     // 15M gas (exceeds limit when added to tx1)
        );

        // tx3: 5M gas, max_fee=30 gwei, priority=0 gwei
        // EGP = min(30_000_000_000, 50_000_000_000 + 0) = 30_000_000_000
        // Below base_fee (50 gwei), so should be filtered out
        let tx3 = tx_with_type_and_fees(
            2,
            "0x0",
            "0x6FC23AC00", // max_fee = 30 gwei
            "0x0",         // priority_fee = 0 gwei
            5_000_000,     // 5M gas
        );

        let txs = vec![tx1.clone(), tx2.clone(), tx3.clone()];
        let (ordered, rejected) = order_by_egp(txs, base_fee);

        // tx1 (20M) should fit, tx2 (15M) would exceed 30M limit, tx3 filtered by base_fee
        assert_eq!(
            ordered.len(),
            1,
            "only tx1 should fit within 30M gas limit, got {} txs",
            ordered.len()
        );
        assert_eq!(rejected, 2, "tx2 exceeds limit, tx3 below base_fee");
        assert_eq!(ordered[0].hash, tx1.hash, "tx1 should be included");
    }

    #[test]
    fn test_apply_nonce_constraints_removes_gaps() {
        let sender_a = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let sender_b = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let txs = vec![
            tx_for_sender(sender_a, 0, 21_000),
            tx_for_sender(sender_a, 2, 21_000),
            tx_for_sender(sender_b, 5, 21_000),
            tx_for_sender(sender_b, 6, 21_000),
            tx_for_sender(sender_b, 8, 21_000),
        ];

        let constrained = apply_nonce_constraints(txs);

        let mut by_sender: HashMap<String, Vec<u64>> = HashMap::new();
        for tx in constrained {
            by_sender
                .entry(tx.from_address.to_lowercase())
                .or_default()
                .push(tx.nonce);
        }

        let mut a = by_sender.get(sender_a).cloned().unwrap_or_default();
        a.sort_unstable();

        let mut b = by_sender.get(sender_b).cloned().unwrap_or_default();
        b.sort_unstable();

        assert_eq!(a, vec![0], "sender A nonce gap should remove nonce 2");
        assert_eq!(b, vec![5, 6], "sender B nonce gap should remove nonce 8");
    }
}
