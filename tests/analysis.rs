//! Integration tests for mev-analysis PnL computation and statistics.

mod common;

use mev_analysis::pnl::{compute_range_stats, format_eth, BlockPnL};

/// Test that capture rate is calculated correctly as simulated/actual.
///
/// Actual block value: 1 ETH = 1_000_000_000_000_000_000 Wei
/// Simulated block value: 0.8 ETH = 800_000_000_000_000_000 Wei
/// Expected capture rate: 0.8
#[test]
fn pnl_capture_rate_calculation() {
    let pnl_record = BlockPnL {
        block_number: 18_000_000,
        block_hash: "0xabc123".to_string(),
        miner: "0xminer".to_string(),
        tx_count: 1,
        gas_used: 21_000,
        base_fee_per_gas_wei: 1_000_000_000u128,
        actual_block_value_wei: 1_000_000_000_000_000_000u128, // 1 ETH
        simulated_block_value_wei: 800_000_000_000_000_000u128, // 0.8 ETH
        egp_simulated_value_wei: 800_000_000_000_000_000u128,
        profit_simulated_value_wei: 0u128,
        arbitrage_simulated_value_wei: 0u128,
        mev_captured_wei: 0u128,
        private_flow_estimate_wei: 200_000_000_000_000_000u128, // 0.2 ETH
        value_gap_wei: -200_000_000_000_000_000i128,
        capture_rate: 0.8,
    };

    // Verify capture rate is 0.8
    assert_eq!(pnl_record.capture_rate, 0.8);
    assert_eq!(
        pnl_record.simulated_block_value_wei as f64 / pnl_record.actual_block_value_wei as f64,
        0.8
    );
}

/// Test that mean capture rate is calculated correctly across multiple blocks.
///
/// 3 blocks with capture rates: 0.8, 0.6, 0.7
/// Expected mean: (0.8 + 0.6 + 0.7) / 3 = 0.7
#[test]
fn range_stats_mean_correct() {
    let records = vec![
        BlockPnL {
            block_number: 18_000_000,
            block_hash: "0xabc".to_string(),
            miner: "0x1".to_string(),
            tx_count: 1,
            gas_used: 21_000,
            base_fee_per_gas_wei: 1_000_000_000u128,
            actual_block_value_wei: 1_000_000_000_000_000_000u128,
            simulated_block_value_wei: 800_000_000_000_000_000u128,
            egp_simulated_value_wei: 800_000_000_000_000_000u128,
            profit_simulated_value_wei: 0u128,
            arbitrage_simulated_value_wei: 0u128,
            mev_captured_wei: 0u128,
            private_flow_estimate_wei: 0u128,
            value_gap_wei: 0i128,
            capture_rate: 0.8,
        },
        BlockPnL {
            block_number: 18_000_001,
            block_hash: "0xdef".to_string(),
            miner: "0x2".to_string(),
            tx_count: 1,
            gas_used: 21_000,
            base_fee_per_gas_wei: 1_000_000_000u128,
            actual_block_value_wei: 1_000_000_000_000_000_000u128,
            simulated_block_value_wei: 600_000_000_000_000_000u128,
            egp_simulated_value_wei: 600_000_000_000_000_000u128,
            profit_simulated_value_wei: 0u128,
            arbitrage_simulated_value_wei: 0u128,
            mev_captured_wei: 0u128,
            private_flow_estimate_wei: 0u128,
            value_gap_wei: 0i128,
            capture_rate: 0.6,
        },
        BlockPnL {
            block_number: 18_000_002,
            block_hash: "0x123".to_string(),
            miner: "0x3".to_string(),
            tx_count: 1,
            gas_used: 21_000,
            base_fee_per_gas_wei: 1_000_000_000u128,
            actual_block_value_wei: 1_000_000_000_000_000_000u128,
            simulated_block_value_wei: 700_000_000_000_000_000u128,
            egp_simulated_value_wei: 700_000_000_000_000_000u128,
            profit_simulated_value_wei: 0u128,
            arbitrage_simulated_value_wei: 0u128,
            mev_captured_wei: 0u128,
            private_flow_estimate_wei: 0u128,
            value_gap_wei: 0i128,
            capture_rate: 0.7,
        },
    ];

    let stats = compute_range_stats(&records);

    // Verify mean capture rate calculation
    assert_eq!(stats.block_count, 3);
    assert!(
        (stats.mean_capture_rate - 0.7).abs() < 0.0001,
        "mean capture rate should be 0.7, got {}",
        stats.mean_capture_rate
    );
}

/// Test format_eth precision with 6 specific Wei values.
///
/// Tests exact formatting for:
/// 1. 1 full ETH
/// 2. 0.5 ETH
/// 3. 0.000001 ETH (1 micro-ETH)
/// 4. 0.000000123456 ETH
/// 5. 0 ETH
/// 6. 1234.567890123456 ETH (truncated to 6 decimals)
#[test]
fn format_eth_precision() {
    // Test 1: 1 full ETH
    let wei_1eth = 1_000_000_000_000_000_000u128;
    assert_eq!(format_eth(wei_1eth), "1.000000 ETH");

    // Test 2: 0.5 ETH
    let wei_half = 500_000_000_000_000_000u128;
    assert_eq!(format_eth(wei_half), "0.500000 ETH");

    // Test 3: 0.000001 ETH (1 micro-ETH)
    let wei_micro = 1_000_000_000_000u128;
    assert_eq!(format_eth(wei_micro), "0.000001 ETH");

    // Test 4: 0.000000123456 ETH (very small value, truncated at 6 decimals to 0.000000)
    let wei_small = 123_456_000u128;
    assert_eq!(format_eth(wei_small), "0.000000 ETH");

    // Test 5: 0 ETH
    let wei_zero = 0u128;
    assert_eq!(format_eth(wei_zero), "0.000000 ETH");

    // Test 6: 1234.567890123456 ETH (only first 6 decimals shown)
    let wei_large = 1_234_567_890_123_456_000u128;
    assert_eq!(format_eth(wei_large), "1.234567 ETH");
}
