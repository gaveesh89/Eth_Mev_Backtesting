//! Integration tests for the Uniswap V3 price reader.
//!
//! Offline tests (no RPC) run by default. RPC-dependent tests are `#[ignore]`
//! and require `MEV_RPC_URL` to be set to an archive node.

use alloy::primitives::U256;
use mev_sim::v3::price::sqrt_price_x96_to_price;
use mev_sim::v3::slot0::V3_WETH_USDC_POOL;

/// Known sqrtPriceX96 from RareSkills example (USDC/ETH pool).
/// Verified via Python: USDC/WETH ≈ $2765.16 at that snapshot.
#[test]
fn test_sqrt_price_x96_to_price_known_values() {
    let sqrt_price =
        U256::from_str_radix("1506673274302120988651364689808458", 10).expect("valid U256");

    // token0 = USDC (6 dec), token1 = WETH (18 dec)
    // is_token0_quote = true → USDC per WETH
    let result = sqrt_price_x96_to_price(sqrt_price, 6, 18, true);

    let divisor = U256::from(10u64).pow(U256::from(result.precision_decimals));
    let integer_part: u64 = (result.price_scaled / divisor).to::<u64>();

    // Python reference: 2765.16 — allow 1% tolerance → [2700, 2830]
    assert!(
        (2700..=2830).contains(&integer_part),
        "Expected price ~2765 USDC/WETH, got {integer_part} (display: {})",
        result.display
    );

    // Verify the display string contains a decimal point
    assert!(
        result.display.contains('.'),
        "Display string should contain decimal point: {}",
        result.display
    );
}

/// Fetch slot0 at block 17,000,000 and 17,000,010 — sqrtPriceX96 should differ.
#[tokio::test]
#[ignore] // requires archive RPC
async fn test_univ3_slot0_changes_across_blocks() {
    let rpc_url = std::env::var("MEV_RPC_URL").expect("MEV_RPC_URL must be set for ignored tests");

    let slot0_a = mev_sim::v3::slot0::fetch_slot0_via_call(&rpc_url, V3_WETH_USDC_POOL, 17_000_000)
        .await
        .expect("fetch slot0 at block 17M");

    let slot0_b = mev_sim::v3::slot0::fetch_slot0_via_call(&rpc_url, V3_WETH_USDC_POOL, 17_000_010)
        .await
        .expect("fetch slot0 at block 17M+10");

    // Price should have moved (or at least one field changed)
    assert_ne!(
        slot0_a.sqrt_price_x96, slot0_b.sqrt_price_x96,
        "sqrtPriceX96 should differ between block 17M and 17M+10"
    );

    // Derived prices should be in sane range: 1000 < USDC/WETH < 5000
    let price_a = sqrt_price_x96_to_price(slot0_a.sqrt_price_x96, 6, 18, true);
    let price_b = sqrt_price_x96_to_price(slot0_b.sqrt_price_x96, 6, 18, true);

    let divisor = U256::from(10u64).pow(U256::from(price_a.precision_decimals));
    let int_a: u64 = (price_a.price_scaled / divisor).to::<u64>();
    let int_b: u64 = (price_b.price_scaled / divisor).to::<u64>();

    assert!(
        (1000..=5000).contains(&int_a),
        "Price at 17M out of sane range: {int_a} (display: {})",
        price_a.display
    );
    assert!(
        (1000..=5000).contains(&int_b),
        "Price at 17M+10 out of sane range: {int_b} (display: {})",
        price_b.display
    );

    // Tick should be consistent with sqrtPriceX96 (both negative for USDC/WETH)
    assert!(
        slot0_a.tick < 0,
        "tick at 17M should be negative for USDC/WETH pool, got {}",
        slot0_a.tick
    );
}

/// Cross-validate: eth_call and eth_getStorageAt must return the same sqrtPriceX96 and tick.
#[tokio::test]
#[ignore] // requires archive RPC
async fn test_univ3_slot0_storage_equals_eth_call() {
    let rpc_url = std::env::var("MEV_RPC_URL").expect("MEV_RPC_URL must be set for ignored tests");

    let via_call =
        mev_sim::v3::slot0::fetch_slot0_via_call(&rpc_url, V3_WETH_USDC_POOL, 17_000_000)
            .await
            .expect("fetch slot0 via eth_call");

    let via_storage =
        mev_sim::v3::slot0::fetch_slot0_via_storage(&rpc_url, V3_WETH_USDC_POOL, 17_000_000)
            .await
            .expect("fetch slot0 via eth_getStorageAt");

    assert_eq!(
        via_call.sqrt_price_x96, via_storage.sqrt_price_x96,
        "sqrtPriceX96 mismatch: call={}, storage={}",
        via_call.sqrt_price_x96, via_storage.sqrt_price_x96
    );

    assert_eq!(
        via_call.tick, via_storage.tick,
        "tick mismatch: call={}, storage={}",
        via_call.tick, via_storage.tick
    );

    assert_eq!(
        via_call.observation_index, via_storage.observation_index,
        "observationIndex mismatch"
    );

    assert_eq!(
        via_call.observation_cardinality, via_storage.observation_cardinality,
        "observationCardinality mismatch"
    );
}
