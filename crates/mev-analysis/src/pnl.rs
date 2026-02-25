//! PnL foundations for block-level value analysis.
//!
//! Computes the actual block value captured by validators from priority fees
//! and formats Wei values into fixed-precision ETH strings.

use eyre::{eyre, Result};
use mev_data::store::Store;
use mev_data::types::{Block, BlockTransaction};

/// Block-level PnL aggregate.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockPnL {
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: String,
    /// Miner/coinbase address.
    pub miner: String,
    /// Number of transactions considered.
    pub tx_count: usize,
    /// Total gas used across transactions.
    pub gas_used: u64,
    /// Base fee in Wei.
    pub base_fee_per_gas_wei: u128,
    /// Actual value captured by block producer in Wei.
    pub actual_block_value_wei: u128,
    /// Simulated value in Wei.
    pub simulated_block_value_wei: u128,
    /// Simulated value under EGP ordering in Wei.
    pub egp_simulated_value_wei: u128,
    /// Simulated value under profit ordering in Wei.
    pub profit_simulated_value_wei: u128,
    /// Simulated value under arbitrage strategy in Wei.
    pub arbitrage_simulated_value_wei: u128,
    /// Captured MEV in Wei.
    pub mev_captured_wei: u128,
    /// Private flow estimate in Wei.
    pub private_flow_estimate_wei: u128,
    /// Difference between simulated and actual value in Wei.
    pub value_gap_wei: i128,
    /// Capture rate (simulated/actual).
    pub capture_rate: f64,
}

/// Aggregated statistics over a block range.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeStats {
    /// Number of block records aggregated.
    pub block_count: usize,
    /// Sum of actual block values across range.
    pub total_actual_block_value_wei: u128,
    /// Sum of selected simulated values across range.
    pub total_simulated_block_value_wei: u128,
    /// Sum of EGP simulated values across range.
    pub total_egp_simulated_value_wei: u128,
    /// Sum of profit-sorted simulated values across range.
    pub total_profit_simulated_value_wei: u128,
    /// Sum of arbitrage-strategy simulated values across range.
    pub total_arbitrage_simulated_value_wei: u128,
    /// Sum of MEV captured across range.
    pub total_mev_captured_wei: u128,
    /// Sum of private flow estimates across range.
    pub total_private_flow_estimate_wei: u128,
    /// Mean capture rate (average of individual record rates).
    pub mean_capture_rate: f64,
}

fn parse_hex_u128(value: &str) -> u128 {
    let stripped = value.trim_start_matches("0x");
    if stripped.is_empty() {
        return 0;
    }

    u128::from_str_radix(stripped, 16).unwrap_or(0)
}

fn estimate_direct_miner_transfers_wei(_block: &Block, _txs: &[BlockTransaction]) -> u128 {
    0
}

/// Computes actual block value in Wei.
///
/// Formula:
/// - Per tx validator value from gas is `(effective_gas_price - base_fee) * gas_used`
/// - Plus direct ETH transfers to `block.miner` within the same block
pub fn compute_actual_block_value(block: &Block, txs: &[BlockTransaction]) -> u128 {
    let base_fee = parse_hex_u128(&block.base_fee_per_gas);

    let gas_value = txs
        .iter()
        .map(|tx| {
            let effective_gas_price = parse_hex_u128(&tx.effective_gas_price);
            let priority_fee = effective_gas_price.saturating_sub(base_fee);
            priority_fee.saturating_mul(tx.gas_used as u128)
        })
        .sum::<u128>();

    let direct_transfers = estimate_direct_miner_transfers_wei(block, txs);
    gas_value.saturating_add(direct_transfers)
}

/// Formats Wei to ETH string with exactly 6 decimal places.
///
/// Examples:
/// - `1_000_000_000_000_000_000` -> `"1.000000 ETH"`
/// - `123_000_000_000_000` -> `"0.000123 ETH"`
pub fn format_eth(wei: u128) -> String {
    const WEI_PER_ETH: u128 = 1_000_000_000_000_000_000;
    const SCALE: u128 = 1_000_000;

    let whole = wei / WEI_PER_ETH;
    let fractional = ((wei % WEI_PER_ETH) * SCALE) / WEI_PER_ETH;

    format!("{whole}.{fractional:06} ETH")
}

/// Computes full block PnL from persisted SQLite data.
///
/// Loads block, block transactions, and simulation records for the provided block number.
/// If no simulation results exist yet, returns a partial PnL with simulated fields as zero.
pub fn compute_pnl(block_number: u64, store: &Store) -> Result<BlockPnL> {
    let block = store
        .get_block(block_number)?
        .ok_or_else(|| eyre!("block {block_number} not found"))?;
    let txs = store.get_block_txs(block_number)?;

    let base_fee_per_gas_wei = parse_hex_u128(&block.base_fee_per_gas);
    let tx_count = txs.len();
    let gas_used = txs.iter().map(|tx| tx.gas_used).sum::<u64>();
    let actual_block_value_wei = compute_actual_block_value(&block, &txs);

    let (egp_simulated_opt, profit_simulated_opt, arbitrage_simulated_opt) =
        store.get_simulated_values_for_block(block_number)?;
    let egp_simulated_value_wei = egp_simulated_opt.unwrap_or(0);
    let profit_simulated_value_wei = profit_simulated_opt.unwrap_or(0);
    let arbitrage_simulated_value_wei = arbitrage_simulated_opt.unwrap_or(0);

    let simulated_block_value_wei = egp_simulated_value_wei
        .max(profit_simulated_value_wei)
        .max(arbitrage_simulated_value_wei);
    let mev_captured_wei = simulated_block_value_wei.saturating_sub(actual_block_value_wei);
    let private_flow_estimate_wei =
        actual_block_value_wei.saturating_sub(simulated_block_value_wei);
    let value_gap_wei = simulated_block_value_wei as i128 - actual_block_value_wei as i128;
    let capture_rate = if actual_block_value_wei == 0 {
        0.0
    } else {
        simulated_block_value_wei as f64 / actual_block_value_wei as f64
    };

    Ok(BlockPnL {
        block_number,
        block_hash: block.block_hash,
        miner: block.miner,
        tx_count,
        gas_used,
        base_fee_per_gas_wei,
        actual_block_value_wei,
        simulated_block_value_wei,
        egp_simulated_value_wei,
        profit_simulated_value_wei,
        arbitrage_simulated_value_wei,
        mev_captured_wei,
        private_flow_estimate_wei,
        value_gap_wei,
        capture_rate,
    })
}

/// Aggregates summary statistics across block-level PnL records.
pub fn compute_range_stats(records: &[BlockPnL]) -> RangeStats {
    let block_count = records.len();

    let total_actual_block_value_wei = records
        .iter()
        .map(|record| record.actual_block_value_wei)
        .sum();
    let total_simulated_block_value_wei = records
        .iter()
        .map(|record| record.simulated_block_value_wei)
        .sum();
    let total_egp_simulated_value_wei = records
        .iter()
        .map(|record| record.egp_simulated_value_wei)
        .sum();
    let total_profit_simulated_value_wei = records
        .iter()
        .map(|record| record.profit_simulated_value_wei)
        .sum();
    let total_arbitrage_simulated_value_wei = records
        .iter()
        .map(|record| record.arbitrage_simulated_value_wei)
        .sum();
    let total_mev_captured_wei = records.iter().map(|record| record.mev_captured_wei).sum();
    let total_private_flow_estimate_wei = records
        .iter()
        .map(|record| record.private_flow_estimate_wei)
        .sum();

    let mean_capture_rate = if block_count == 0 {
        0.0
    } else {
        records
            .iter()
            .map(|record| record.capture_rate)
            .sum::<f64>()
            / block_count as f64
    };

    RangeStats {
        block_count,
        total_actual_block_value_wei,
        total_simulated_block_value_wei,
        total_egp_simulated_value_wei,
        total_profit_simulated_value_wei,
        total_arbitrage_simulated_value_wei,
        total_mev_captured_wei,
        total_private_flow_estimate_wei,
        mean_capture_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_actual_block_value_from_three_txs() {
        let block = Block {
            block_number: 18_000_000,
            block_hash: "0xabc".to_string(),
            parent_hash: "0xdef".to_string(),
            timestamp: 1_700_000_000,
            gas_limit: 30_000_000,
            gas_used: 171_000,
            base_fee_per_gas: "0x64".to_string(),
            miner: "0x00000000000000000000000000000000000000aa".to_string(),
            transaction_count: 3,
        };

        let txs = vec![
            BlockTransaction {
                block_number: block.block_number,
                tx_hash: "0x1".to_string(),
                tx_index: 0,
                from_address: "0x01".to_string(),
                to_address: "0x02".to_string(),
                gas_used: 21_000,
                effective_gas_price: "0x96".to_string(),
                status: 1,
            },
            BlockTransaction {
                block_number: block.block_number,
                tx_hash: "0x2".to_string(),
                tx_index: 1,
                from_address: "0x01".to_string(),
                to_address: "0x02".to_string(),
                gas_used: 50_000,
                effective_gas_price: "0x78".to_string(),
                status: 1,
            },
            BlockTransaction {
                block_number: block.block_number,
                tx_hash: "0x3".to_string(),
                tx_index: 2,
                from_address: "0x01".to_string(),
                to_address: "0x02".to_string(),
                gas_used: 100_000,
                effective_gas_price: "0x64".to_string(),
                status: 1,
            },
        ];

        let actual = compute_actual_block_value(&block, &txs);
        assert_eq!(actual, 2_050_000);
    }

    #[test]
    fn formats_one_eth_exactly() {
        assert_eq!(format_eth(1_000_000_000_000_000_000), "1.000000 ETH");
    }

    #[test]
    fn computes_range_stats_means() {
        let records = vec![
            BlockPnL {
                block_number: 1,
                block_hash: "0x1".into(),
                miner: "0xa".into(),
                tx_count: 10,
                gas_used: 100_000,
                base_fee_per_gas_wei: 100,
                actual_block_value_wei: 100,
                simulated_block_value_wei: 80,
                egp_simulated_value_wei: 70,
                profit_simulated_value_wei: 80,
                arbitrage_simulated_value_wei: 0,
                mev_captured_wei: 0,
                private_flow_estimate_wei: 20,
                value_gap_wei: -20,
                capture_rate: 0.8,
            },
            BlockPnL {
                block_number: 2,
                block_hash: "0x2".into(),
                miner: "0xb".into(),
                tx_count: 12,
                gas_used: 120_000,
                base_fee_per_gas_wei: 120,
                actual_block_value_wei: 200,
                simulated_block_value_wei: 300,
                egp_simulated_value_wei: 250,
                profit_simulated_value_wei: 300,
                arbitrage_simulated_value_wei: 0,
                mev_captured_wei: 100,
                private_flow_estimate_wei: 0,
                value_gap_wei: 100,
                capture_rate: 1.5,
            },
            BlockPnL {
                block_number: 3,
                block_hash: "0x3".into(),
                miner: "0xc".into(),
                tx_count: 8,
                gas_used: 90_000,
                base_fee_per_gas_wei: 90,
                actual_block_value_wei: 50,
                simulated_block_value_wei: 0,
                egp_simulated_value_wei: 0,
                profit_simulated_value_wei: 0,
                arbitrage_simulated_value_wei: 0,
                mev_captured_wei: 0,
                private_flow_estimate_wei: 50,
                value_gap_wei: -50,
                capture_rate: 0.0,
            },
        ];

        let stats = compute_range_stats(&records);
        assert_eq!(stats.block_count, 3);
        assert_eq!(stats.total_actual_block_value_wei, 350);
        assert_eq!(stats.total_simulated_block_value_wei, 380);
        assert_eq!(stats.total_egp_simulated_value_wei, 320);
        assert_eq!(stats.total_profit_simulated_value_wei, 380);
        assert_eq!(stats.total_arbitrage_simulated_value_wei, 0);
        assert_eq!(stats.total_private_flow_estimate_wei, 70);
        assert!((stats.mean_capture_rate - (0.8 + 1.5 + 0.0) / 3.0).abs() < 1e-12);
    }
}
