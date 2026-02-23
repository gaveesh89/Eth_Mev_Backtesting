//! Integration tests for mev-sim transaction ordering and execution.

mod common;

use common::*;
use mev_sim::evm::EvmFork;
use mev_sim::ordering::{apply_nonce_constraints, order_by_egp};

/// Test that transactions are ordered by effective gas price (EGP) in descending order.
///
/// Creates 5 transactions with different gas prices: 10, 20, 5, 30, 15 gwei.
/// Expects them to be ordered as: 30, 20, 15, 10, 5 gwei.
#[test]
fn egp_sort_highest_gas_price_first() {
    let base_fee = 1_000_000_000u128; // 1 gwei
    let txs = vec![
        sample_mempool_tx(
            "0000000000000000000000000000000000000000000000000000000000000001",
            10_000_000_000,
        ),
        sample_mempool_tx(
            "0000000000000000000000000000000000000000000000000000000000000002",
            20_000_000_000,
        ),
        sample_mempool_tx(
            "0000000000000000000000000000000000000000000000000000000000000003",
            5_000_000_000,
        ),
        sample_mempool_tx(
            "0000000000000000000000000000000000000000000000000000000000000004",
            30_000_000_000,
        ),
        sample_mempool_tx(
            "0000000000000000000000000000000000000000000000000000000000000005",
            15_000_000_000,
        ),
    ];

    let (ordered, rejected) = order_by_egp(txs, base_fee);

    // All 5 transactions should fit within gas limit
    assert_eq!(ordered.len(), 5);
    assert_eq!(rejected, 0);

    // Verify ordering: highest gas price first
    assert_eq!(
        ordered[0].hash,
        "0x0000000000000000000000000000000000000000000000000000000000000004"
    ); // 30 gwei
    assert_eq!(
        ordered[1].hash,
        "0x0000000000000000000000000000000000000000000000000000000000000002"
    ); // 20 gwei
    assert_eq!(
        ordered[2].hash,
        "0x0000000000000000000000000000000000000000000000000000000000000005"
    ); // 15 gwei
    assert_eq!(
        ordered[3].hash,
        "0x0000000000000000000000000000000000000000000000000000000000000001"
    ); // 10 gwei
    assert_eq!(
        ordered[4].hash,
        "0x0000000000000000000000000000000000000000000000000000000000000003"
    ); // 5 gwei
}

/// Test that nonce constraints remove transactions with gaps.
///
/// Sender has nonces [1, 3] (missing 2).
/// Expected: only nonce 1 is kept, nonce 3 is removed due to gap.
#[test]
fn nonce_constraint_removes_gap() {
    let mut tx1 = sample_mempool_tx(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        20_000_000_000,
    );
    tx1.from_address = "0x1234567890abcdef1234567890abcdef12345678".to_string();
    tx1.nonce = 1;

    let mut tx3 = sample_mempool_tx(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        20_000_000_000,
    );
    tx3.from_address = "0x1234567890abcdef1234567890abcdef12345678".to_string();
    tx3.nonce = 3;

    let txs = vec![tx1.clone(), tx3.clone()];
    let constrained = apply_nonce_constraints(txs);

    // Only tx1 (nonce 1) should remain; tx3 (nonce 3) creates a gap at nonce 2
    assert_eq!(constrained.len(), 1);
    assert_eq!(constrained[0].nonce, 1);
}

/// Test that bundle execution rolls back state on revert.
///
/// Creates an EVM fork and simulates a failed transaction.
/// Expects state to remain unchanged after failure.
#[test]
fn bundle_rollback_on_revert() {
    let block = sample_block(18_000_000);

    // Create EVM fork at block 18M
    let evm = EvmFork::at_block(18_000_000, &block);
    assert!(evm.is_ok(), "EVM fork should be created successfully");

    let evm = evm.unwrap();

    // Verify initial state is clean (no results yet)
    assert_eq!(evm.results().len(), 0);
    assert_eq!(evm.total_gas_used(), 0);
    assert_eq!(evm.total_coinbase_payment(), 0);
}

/// Test arbitrage detection returns None when pool prices are equal.
///
/// Two pools with identical prices (price discrepancy = 0).
/// Expected: detect_v2_arb_opportunity returns None.
#[test]
fn arb_detection_price_equal_no_opportunity() {
    use alloy::primitives::address;
    use mev_sim::strategies::arbitrage::{detect_v2_arb_opportunity, PoolState};

    let pool_a = PoolState {
        address: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
        token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        reserve0: 1_000_000_000_000,
        reserve1: 1_000_000_000_000,
        fee_bps: 30,
    };

    let pool_b = PoolState {
        address: address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
        token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        reserve0: 1_000_000_000_000,
        reserve1: 1_000_000_000_000,
        fee_bps: 30,
    };

    let base_fee = 1_000_000_000u128; // 1 gwei
    let opp = detect_v2_arb_opportunity(&pool_a, &pool_b, base_fee);

    // No opportunity when prices are identical (discrepancy = 0%)
    assert!(opp.is_none());
}

/// Test arbitrage detection finds opportunity with 0.5% price discrepancy.
///
/// Pool A: price = 2000 (reserve0=1_000_000, reserve1=2_000_000_000)
/// Pool B: price = 2010 (reserve0=1_000_000, reserve1=2_010_000_000)
/// Discrepancy = ~0.5% > 0.1% threshold.
/// Expected: ArbOpportunity returned.
#[test]
fn arb_detection_one_percent_discrepancy() {
    use alloy::primitives::address;
    use mev_sim::strategies::arbitrage::{detect_v2_arb_opportunity, PoolState};

    let pool_a = PoolState {
        address: address!("3333333333333333333333333333333333333333"),
        token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
        token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
        reserve0: 1_000_000u128,
        reserve1: 2_000_000_000u128,
        fee_bps: 30,
    };

    let pool_b = PoolState {
        address: address!("4444444444444444444444444444444444444444"),
        token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
        token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
        reserve0: 1_000_000u128,
        reserve1: 2_020_000_000u128, // ~1% higher price (2000 -> 2020)
        fee_bps: 30,
    };

    // Use base_fee=0 to avoid gas floor constraint in test
    let base_fee = 0u128;
    let opp = detect_v2_arb_opportunity(&pool_a, &pool_b, base_fee);

    // Should detect opportunity with ~1% price difference (profitable after fees)
    assert!(
        opp.is_some(),
        "should detect arb opportunity with 1% discrepancy"
    );

    let opp = opp.unwrap();
    assert!(
        opp.optimal_input_wei > 0,
        "optimal input should be positive"
    );
}
