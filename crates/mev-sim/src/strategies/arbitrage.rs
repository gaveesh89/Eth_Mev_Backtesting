//! Uniswap V2 top-of-block arbitrage detection with rigorous math.
//!
//! Constant-product pools maintain an invariant where reserve0 × reserve1
//! stays approximately constant after each swap. If two pools for the same
//! token pair imply different prices, you can buy from the cheaper pool and
//! sell into the more expensive one in a two-leg cycle.
//!
//! **Key Improvements:**
//! - Closed-form optimal input formula (fee-adjusted, Flashbots-style)
//! - Ternary search fallback for ineligible fee combinations
//! - Gas-aware profit calculation (net_profit = amm_profit - gas_cost_in_tokens)
//! - Overflow-safe U256 intermediate calculations
//! - Rejection thresholds:
//!   * Minimum price discrepancy: 0.1% (10 bps)
//!   * Minimum net profit: > gas_floor
//!   * Only eligible if closed-form applies or ternary search succeeds

use alloy::hex;
use alloy::primitives::{Address, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use eyre::Result;
use mev_data::types::MempoolTransaction;

use crate::decoder::addresses;
use crate::evm::EvmFork;

sol! {
    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }
}

/// Known WETH/stablecoin candidate pools used by default scanning.
///
/// Format: `(pool_address, token0, token1)`.
pub const KNOWN_POOLS: [(Address, Address, Address); 10] = [
    // Uniswap V2 canonical pools.
    (
        alloy::primitives::address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
        addresses::USDC,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"),
        addresses::WETH,
        addresses::USDT,
    ),
    (
        alloy::primitives::address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"),
        addresses::DAI,
        addresses::WETH,
    ),
    // Additional candidate slots for WETH/stable pools on V2-like deployments.
    (
        alloy::primitives::address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
        addresses::USDC,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("06da0fd433C1A5d7a4faa01111c044910A184553"),
        addresses::USDT,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("C3D03e4f041Fd4A6fA2f4f9A31f7fD6A6D1BfA32"),
        addresses::DAI,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("0000000000000000000000000000000000000001"),
        addresses::USDC,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("0000000000000000000000000000000000000002"),
        addresses::USDT,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("0000000000000000000000000000000000000003"),
        addresses::DAI,
        addresses::WETH,
    ),
    (
        alloy::primitives::address!("0000000000000000000000000000000000000004"),
        addresses::USDC,
        addresses::USDT,
    ),
];

/// Snapshot of a Uniswap V2-like pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 address.
    pub token0: Address,
    /// Token1 address.
    pub token1: Address,
    /// Reserve for token0.
    pub reserve0: u128,
    /// Reserve for token1.
    pub reserve1: u128,
    /// Fee in basis points.
    pub fee_bps: u32,
}

/// Estimated arbitrage opportunity between two pools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArbOpportunity {
    /// Input/output base token for the round-trip.
    pub token_a: Address,
    /// Intermediate token for the round-trip.
    pub token_b: Address,
    /// First pool used in the route.
    pub pool_1: Address,
    /// Second pool used in the route.
    pub pool_2: Address,
    /// Gross AMM profit (leg2_output - input) in wei of input token.
    pub gross_profit_wei: u128,
    /// Gas cost converted to input token units.
    pub gas_cost_wei: u128,
    /// Net profit after gas: (gross - gas_cost). Guaranteed > 0.
    pub net_profit_wei: u128,
    /// Estimated optimal input in wei.
    pub optimal_input_wei: u128,
    /// Token path of the arbitrage route.
    pub trade_path: Vec<Address>,
}

fn lookup_pool_tokens(pool: Address) -> Option<(Address, Address)> {
    KNOWN_POOLS
        .iter()
        .find(|(addr, _, _)| *addr == pool)
        .map(|(_, token0, token1)| (*token0, *token1))
}

fn build_readonly_call_tx(to: Address, input_hex: String) -> MempoolTransaction {
    MempoolTransaction {
        hash: format!("0x{:064x}", to),
        block_number: None,
        timestamp_ms: 0,
        from_address: "0x0000000000000000000000000000000000000001".to_string(),
        to_address: Some(format!("{to:#x}")),
        value: "0x0".to_string(),
        gas_limit: 200_000,
        gas_price: "0x1".to_string(),
        max_fee_per_gas: "0x1".to_string(),
        max_priority_fee_per_gas: "0x1".to_string(),
        nonce: 0,
        input_data: input_hex,
        tx_type: 0,
        raw_tx: "0x".to_string(),
    }
}

/// Computes swap output using exact Uniswap V2 integer math (floor division).
///
/// Formula: `(amount_in * 997 * reserve_out) / (reserve_in * 1000 + amount_in * 997)`
///
/// Uses U256 to avoid intermediate overflow on large reserves.
#[inline]
fn amount_out(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }

    let amount_in_u256 = U256::from(amount_in);
    let reserve_in_u256 = U256::from(reserve_in);
    let reserve_out_u256 = U256::from(reserve_out);

    let amount_in_with_fee = amount_in_u256 * U256::from(997u32);
    let numerator = amount_in_with_fee * reserve_out_u256;
    let denominator = (reserve_in_u256 * U256::from(1000u32)) + amount_in_with_fee;

    if denominator.is_zero() {
        return 0;
    }

    (numerator / denominator).to::<u128>()
}

/// Checks if closed-form eligibility applies (both pools share same fee structure).
#[inline]
fn is_closed_form_eligible(pool_a: &PoolState, pool_b: &PoolState) -> bool {
    // Standard V2 fee is 30 bps (0.3%) = 997/1000 multiplier
    pool_a.fee_bps == pool_b.fee_bps && pool_a.fee_bps == 30
}

/// Computes price discrepancy using rational arithmetic (no floats).
///
/// Returns true if `|p_a - p_b| / min(p_a, p_b) > threshold_bps / 10000`
/// using cross-multiplication: `(p_a - p_b).abs() * 10000 > threshold_bps * min(p_a, p_b)`
/// = `|r1_a * r0_b - r1_b * r0_a| * 10000 > threshold_bps * min(r1_a * r0_b, r1_b * r0_a)`
#[inline]
fn exceeds_discrepancy_threshold(
    pool_a: &PoolState,
    pool_b: &PoolState,
    threshold_bps: u128,
) -> bool {
    if pool_a.reserve0 == 0 || pool_a.reserve1 == 0 || pool_b.reserve0 == 0 || pool_b.reserve1 == 0
    {
        return false;
    }

    let r0a = U256::from(pool_a.reserve0);
    let r1a = U256::from(pool_a.reserve1);
    let r0b = U256::from(pool_b.reserve0);
    let r1b = U256::from(pool_b.reserve1);

    let p_a_num = r1a * r0b;
    let p_b_num = r1b * r0a;
    let min_price = p_a_num.min(p_b_num);

    let discrepancy_num = if p_a_num >= p_b_num {
        p_a_num - p_b_num
    } else {
        p_b_num - p_a_num
    };

    // Check: discrepancy_num * 10000 > threshold_bps * min_price
    discrepancy_num * U256::from(10_000u32) > U256::from(threshold_bps) * min_price
}

/// Computes optimal input for standard V2 (fee = 30 bps) using closed-form formula.
///
/// Formula: `optimal_input = (sqrt(f² × r_out_a × r_out_b) - d × r_in_b × r_in_a) × d / (f × r_in_b × d + f² × r_out_a)`
/// where f = 997, d = 1000.
///
/// References: Flashbots MEV-Inspect, exact U256 computation.
fn optimal_input_closed_form(pool_buy: &PoolState, pool_sell: &PoolState) -> u128 {
    if pool_buy.reserve0 == 0
        || pool_buy.reserve1 == 0
        || pool_sell.reserve0 == 0
        || pool_sell.reserve1 == 0
    {
        return 0;
    }

    let f = U256::from(997u32);
    let d = U256::from(1000u32);

    let r_in_a = U256::from(pool_buy.reserve0);
    let r_out_a = U256::from(pool_buy.reserve1);
    let r_in_b = U256::from(pool_sell.reserve1);
    let r_out_b = U256::from(pool_sell.reserve0);

    // presqrt = f² × r_out_a × r_out_b / (r_in_a × r_in_b)
    let presqrt_num = f * f * r_out_a * r_out_b;
    let presqrt_den = r_in_a * r_in_b;

    if presqrt_den.is_zero() {
        return 0;
    }

    // Use integer square root approximation
    let presqrt = presqrt_num / presqrt_den;
    let sqrt_presqrt = isqrt(presqrt);

    if sqrt_presqrt.is_zero() {
        return 0;
    }

    // numerator = (sqrt(presqrt) - d) × r_in_b × r_in_a
    // Check: sqrt_presqrt >= d (otherwise negative result)
    if sqrt_presqrt < d {
        return 0; // Ineligible: closed-form would produce negative input
    }

    let numerator = (sqrt_presqrt - d) * r_in_b * r_in_a;

    // denominator = f × r_in_b × d + f² × r_out_a
    let denominator = f * r_in_b * d + f * f * r_out_a;

    if denominator.is_zero() {
        return 0;
    }

    // optimal_input = numerator × d / denominator
    let result = (numerator * d) / denominator;
    result.to::<u128>()
}

/// Integer square root using Newton's method (ceiling).
#[inline]
fn isqrt(n: U256) -> U256 {
    if n.is_zero() {
        return U256::ZERO;
    }

    let mut x = (n + U256::from(1u32)) >> 1u32;
    let mut y = n;

    while x < y {
        y = x;
        x = (x + n / x) >> 1u32;
    }

    x
}

/// Ternary search over input range to find optimal input.
///
/// Searches interval [1, max_input] for the input that maximizes profit.
/// Terminates when `high - low <= 2`, then linearly checks remaining points.
/// Returns (optimal_input, profit) of the best candidate.
fn ternary_search_optimal_input(
    pool_1: &PoolState,
    pool_2: &PoolState,
    max_input: u128,
) -> (u128, u128) {
    if max_input < 2 {
        return (max_input, estimate_profit(max_input, pool_1, pool_2));
    }

    let mut low: u128 = 1;
    let mut high: u128 = max_input;
    let mut best_input = 1;
    let mut best_profit = estimate_profit(1, pool_1, pool_2);

    while high - low > 2 {
        let mid1 = low + (high - low) / 3;
        let mid2 = high - (high - low) / 3;

        let profit_mid1 = estimate_profit(mid1, pool_1, pool_2);
        let profit_mid2 = estimate_profit(mid2, pool_1, pool_2);

        if profit_mid1 > best_profit {
            best_profit = profit_mid1;
            best_input = mid1;
        }
        if profit_mid2 > best_profit {
            best_profit = profit_mid2;
            best_input = mid2;
        }

        if profit_mid1 > profit_mid2 {
            high = mid2;
        } else {
            low = mid1;
        }
    }

    // Linear check remaining points
    for i in low..=high {
        let profit = estimate_profit(i, pool_1, pool_2);
        if profit > best_profit {
            best_profit = profit;
            best_input = i;
        }
    }

    (best_input, best_profit)
}

/// Estimates profit for a given input amount.
///
/// Returns gross profit: `leg_2_output - input`.
/// Does NOT account for gas costs (use detect_v2_arb_opportunity for that).
fn estimate_profit(input: u128, pool_1: &PoolState, pool_2: &PoolState) -> u128 {
    if input == 0 {
        return 0;
    }

    let leg_1_out = amount_out(input, pool_1.reserve0, pool_1.reserve1);
    if leg_1_out == 0 {
        return 0;
    }

    let leg_2_out = amount_out(leg_1_out, pool_2.reserve1, pool_2.reserve0);
    leg_2_out.saturating_sub(input)
}

/// Detects a two-pool V2 arbitrage opportunity with rigorous math.
///
/// **Key Features:**
/// - Closed-form optimal input (fee-adjusted) if both pools use same fee (30 bps)
/// - Ternary search fallback for mixed fee structures
/// - Gas-aware profit calculation
/// - U256 overflow-safe intermediate calculations
///
/// **Rejection Criteria:**
/// - Pools have incompatible tokens
/// - Price discrepancy <= 0.1% (10 bps)
/// - Net profit (after gas) <= 0
/// - Closed-form ineligible AND ternary search produces no profit
pub fn detect_v2_arb_opportunity(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
) -> Option<ArbOpportunity> {
    // Token compatibility check
    if pool_a.token0 != pool_b.token0 || pool_a.token1 != pool_b.token1 {
        return None;
    }

    // Price discrepancy threshold: must exceed 0.1% (10 bps)
    const DISCREPANCY_THRESHOLD_BPS: u128 = 10;
    if !exceeds_discrepancy_threshold(pool_a, pool_b, DISCREPANCY_THRESHOLD_BPS) {
        tracing::debug!(
            pool_a = %pool_a.address,
            pool_b = %pool_b.address,
            "price discrepancy <= 10 bps; skipping"
        );
        return None;
    }

    tracing::debug!(
        pool_a_r0 = pool_a.reserve0,
        pool_a_r1 = pool_a.reserve1,
        pool_b_r0 = pool_b.reserve0,
        pool_b_r1 = pool_b.reserve1,
        "discrepancy check passed; proceeding with sizing"
    );

    let gas_cost_wei = 200_000u128.saturating_mul(base_fee);

    // **Direction 1: A → B (buy from A, sell to B)**
    let (input_ab, profit_ab) = if is_closed_form_eligible(pool_a, pool_b) {
        let input = optimal_input_closed_form(pool_a, pool_b);
        if input > 0 {
            tracing::debug!(pool_a_b_optimal_input = input, "closed-form (A→B) computed");
            let profit = estimate_profit(input, pool_a, pool_b);
            tracing::debug!(pool_a_b_profit = profit, "profit (A→B) estimated");
            (input, profit)
        } else {
            // Closed-form returned 0: fall back to ternary search
            let max_input = pool_a.reserve0.saturating_mul(10) / 100; // 10% of reserves
            tracing::debug!(
                pool_a_b_max_input = max_input,
                "closed-form infeasible; fallback to ternary search (A→B)"
            );
            ternary_search_optimal_input(pool_a, pool_b, max_input)
        }
    } else {
        // Fallback: ternary search
        let max_input = pool_a.reserve0.saturating_mul(10) / 100; // 10% of reserves
        tracing::debug!(
            pool_a_b_max_input = max_input,
            "fallback to ternary search (A→B)"
        );
        ternary_search_optimal_input(pool_a, pool_b, max_input)
    };

    // **Direction 2: B → A (buy from B, sell to A)**
    let (input_ba, profit_ba) = if is_closed_form_eligible(pool_b, pool_a) {
        let input = optimal_input_closed_form(pool_b, pool_a);
        if input > 0 {
            tracing::debug!(pool_b_a_optimal_input = input, "closed-form (B→A) computed");
            let profit = estimate_profit(input, pool_b, pool_a);
            tracing::debug!(pool_b_a_profit = profit, "profit (B→A) estimated");
            (input, profit)
        } else {
            // Closed-form returned 0: fall back to ternary search
            let max_input = pool_b.reserve0.saturating_mul(10) / 100; // 10% of reserves
            tracing::debug!(
                pool_b_a_max_input = max_input,
                "closed-form infeasible; fallback to ternary search (B→A)"
            );
            ternary_search_optimal_input(pool_b, pool_a, max_input)
        }
    } else {
        // Fallback: ternary search
        let max_input = pool_b.reserve0.saturating_mul(10) / 100; // 10% of reserves
        tracing::debug!(
            pool_b_a_max_input = max_input,
            "fallback to ternary search (B→A)"
        );
        ternary_search_optimal_input(pool_b, pool_a, max_input)
    };

    // Pick the more profitable direction
    let (pool_1, pool_2, optimal_input_wei, gross_profit_wei) = if profit_ab > profit_ba {
        (pool_a.address, pool_b.address, input_ab, profit_ab)
    } else if profit_ba > profit_ab {
        (pool_b.address, pool_a.address, input_ba, profit_ba)
    } else {
        // Same profit: prefer larger input
        if input_ab >= input_ba {
            (pool_a.address, pool_b.address, input_ab, profit_ab)
        } else {
            (pool_b.address, pool_a.address, input_ba, profit_ba)
        }
    };

    // Reject if input is zero or gross profit insufficient
    if optimal_input_wei == 0 || gross_profit_wei == 0 {
        tracing::debug!(
            optimal_input_wei,
            gross_profit_wei,
            "rejected: zero input or profit"
        );
        return None;
    }

    // **Net profit = gross profit - gas cost (in input token units)**
    // NOTE: Simplified assumption — gas cost already in input token.
    // In production, would convert via reference pool (e.g., token → WETH → baseFee).
    let net_profit_wei = match gross_profit_wei.checked_sub(gas_cost_wei) {
        Some(np) if np > 0 => np,
        _ => {
            tracing::debug!(
                gross_profit_wei,
                gas_cost_wei,
                "net profit non-positive after gas; rejecting"
            );
            return None;
        }
    };

    tracing::debug!(
        pool_1 = %pool_1,
        pool_2 = %pool_2,
        optimal_input_wei,
        gross_profit_wei,
        net_profit_wei,
        "opportunity detected"
    );

    Some(ArbOpportunity {
        token_a: pool_a.token0,
        token_b: pool_a.token1,
        pool_1,
        pool_2,
        gross_profit_wei,
        gas_cost_wei,
        net_profit_wei,
        optimal_input_wei,
        trade_path: vec![pool_a.token0, pool_a.token1, pool_a.token0],
    })
}

/// Fetches pool reserves for Uniswap V2 pairs via read-only REVM simulation.
///
/// Uses `getReserves()` ABI encoding from the `sol!` interface and submits a
/// read-only call transaction through `EvmFork::simulate_tx`.
pub async fn fetch_pool_states(
    pool_addresses: &[Address],
    evm: &mut EvmFork,
) -> Result<Vec<PoolState>> {
    let mut states = Vec::new();
    let call_data = IUniswapV2Pair::getReservesCall {}.abi_encode();
    let call_hex = format!("0x{}", hex::encode(call_data));

    for pool in pool_addresses {
        let Some((token0, token1)) = lookup_pool_tokens(*pool) else {
            tracing::debug!(pool = %format!("{pool:#x}"), "pool not found in KNOWN_POOLS; skipping");
            continue;
        };

        let tx = build_readonly_call_tx(*pool, call_hex.clone());
        let sim = match evm.simulate_tx(&tx) {
            Ok(sim) => sim,
            Err(error) => {
                tracing::debug!(
                    pool = %format!("{pool:#x}"),
                    error = %error,
                    "failed to simulate getReserves; skipping"
                );
                continue;
            }
        };

        if sim.output.is_empty() {
            tracing::debug!(pool = %format!("{pool:#x}"), "empty output from getReserves; skipping");
            continue;
        }

        let decoded = match IUniswapV2Pair::getReservesCall::abi_decode_returns(&sim.output, true) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::debug!(
                    pool = %format!("{pool:#x}"),
                    error = %error,
                    "failed to decode getReserves output; skipping"
                );
                continue;
            }
        };

        states.push(PoolState {
            address: *pool,
            token0,
            token1,
            reserve0: decoded.reserve0.to::<u128>(),
            reserve1: decoded.reserve1.to::<u128>(),
            fee_bps: 30,
        });
    }

    Ok(states)
}

/// Scans candidate pools for arbitrage opportunities.
///
/// - Fetches pool states through read-only `getReserves()` calls
/// - Evaluates all pair combinations sharing at least one token
/// - Returns opportunities sorted by descending estimated profit
pub async fn scan_for_arb(evm: &mut EvmFork, pools: &[Address]) -> Result<Vec<ArbOpportunity>> {
    let states = fetch_pool_states(pools, evm).await?;
    if states.len() < 2 {
        return Ok(Vec::new());
    }

    let base_fee = evm.block_env().basefee.to::<u128>();
    let mut opportunities = Vec::new();

    for i in 0..states.len() {
        for j in (i + 1)..states.len() {
            let a = &states[i];
            let b = &states[j];

            let share_common = a.token0 == b.token0
                || a.token0 == b.token1
                || a.token1 == b.token0
                || a.token1 == b.token1;
            if !share_common {
                continue;
            }

            if let Some(opportunity) = detect_v2_arb_opportunity(a, b, base_fee) {
                opportunities.push(opportunity);
            }
        }
    }

    opportunities.sort_by(|lhs, rhs| rhs.net_profit_wei.cmp(&lhs.net_profit_wei));
    Ok(opportunities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_pool(address: Address, reserve0: u128, reserve1: u128) -> PoolState {
        PoolState {
            address,
            token0: alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
            token1: alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
            reserve0,
            reserve1,
            fee_bps: 30,
        }
    }

    #[test]
    fn equal_prices_no_arb() {
        let pool_a = mk_pool(
            alloy::primitives::address!("1111111111111111111111111111111111111111"),
            1_000_000,
            2_000_000_000,
        );
        let pool_b = mk_pool(
            alloy::primitives::address!("2222222222222222222222222222222222222222"),
            500_000,
            1_000_000_000,
        );

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1);
        assert!(result.is_none());
    }

    #[test]
    fn half_percent_discrepancy_finds_arb() {
        // Pool A price: 2000 (2,000,000,000 / 1,000,000)
        // Pool B price: 2020 (2,020,000,000 / 1,000,000) = 1% discrepancy (profitable after 0.6% fees)
        let pool_a = mk_pool(
            alloy::primitives::address!("3333333333333333333333333333333333333333"),
            1_000_000,
            2_000_000_000,
        );
        let pool_b = mk_pool(
            alloy::primitives::address!("4444444444444444444444444444444444444444"),
            1_000_000,
            2_020_000_000,
        );

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 0);
        assert!(result.is_some(), "1% discrepancy should find opportunity");

        let opportunity = result.expect("expected opportunity for 1% discrepancy");
        assert!(opportunity.optimal_input_wei > 0);
        assert!(opportunity.net_profit_wei > 0);
    }

    #[test]
    fn below_threshold_no_arb() {
        // ~0.05% discrepancy: should be below 0.1% threshold.
        let pool_a = mk_pool(
            alloy::primitives::address!("5555555555555555555555555555555555555555"),
            1_000_000,
            2_000_000_000,
        );
        let pool_b = mk_pool(
            alloy::primitives::address!("6666666666666666666666666666666666666666"),
            1_000_000,
            2_001_000_000,
        );

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1);
        assert!(result.is_none());
    }

    #[test]
    fn amount_out_matches_uniswap_v2_semantics() {
        // Test: amount_out(1000, 10_000, 10_000) should match exact formula
        let result = amount_out(1000, 10_000, 10_000);
        // Exact: (1000 * 997 * 10_000) / (10_000 * 1000 + 1000 * 997)
        //      = (9_970_000) / (10_997_000) = 906 (floor)
        assert_eq!(result, 906, "amount_out should use floor division");
    }

    #[test]
    fn closed_form_finds_optimal_input() {
        // Create two pools with 1% discrepancy and same fee structure
        let pool_a = mk_pool(
            alloy::primitives::address!("7777777777777777777777777777777777777777"),
            1_000_000,
            2_000_000_000,
        );
        let pool_b = mk_pool(
            alloy::primitives::address!("8888888888888888888888888888888888888888"),
            1_000_000,
            2_020_000_000,
        );

        // Verify closed-form is eligible
        assert!(is_closed_form_eligible(&pool_a, &pool_b));

        // Closed-form should find optimal input in at least one direction
        let opt_input_ab = optimal_input_closed_form(&pool_a, &pool_b);
        let opt_input_ba = optimal_input_closed_form(&pool_b, &pool_a);

        let opt_input = opt_input_ab.max(opt_input_ba);
        assert!(
            opt_input > 0,
            "closed-form should find positive optimal input"
        );

        // Verify the profit is positive at this input
        let profit = if opt_input == opt_input_ab {
            estimate_profit(opt_input, &pool_a, &pool_b)
        } else {
            estimate_profit(opt_input, &pool_b, &pool_a)
        };
        assert!(profit > 0, "profit at optimal input should be positive");
    }
}
