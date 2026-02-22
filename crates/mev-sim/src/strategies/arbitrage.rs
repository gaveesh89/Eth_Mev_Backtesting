//! Uniswap V2 top-of-block arbitrage detection.
//!
//! Constant-product pools maintain an invariant where reserve0 × reserve1
//! stays approximately constant after each swap. If two pools for the same
//! token pair imply different prices, you can buy from the cheaper pool and
//! sell into the more expensive one in a two-leg cycle.
//!
//! The detector estimates optimal input and round-trip profit using the
//! formulas in `spec.md` and rejects low-signal opportunities using:
//! - a minimum price discrepancy threshold (0.1%), and
//! - a gas floor (`200_000 * base_fee`).

use alloy::hex;
use alloy::primitives::Address;
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
    /// Estimated gross profit in wei.
    pub profit_estimate_wei: u128,
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

fn amount_out(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }

    let amount_in_with_fee = amount_in.saturating_mul(997);
    let numerator = amount_in_with_fee.saturating_mul(reserve_out);
    let denominator = reserve_in
        .saturating_mul(1000)
        .saturating_add(amount_in_with_fee);

    if denominator == 0 {
        return 0;
    }

    numerator / denominator
}

fn relative_discrepancy_bps(pool_a: &PoolState, pool_b: &PoolState) -> u128 {
    if pool_a.reserve0 == 0 || pool_a.reserve1 == 0 || pool_b.reserve0 == 0 || pool_b.reserve1 == 0 {
        return 0;
    }

    // p = reserve1 / reserve0
    let p_a = (pool_a.reserve1 as f64) / (pool_a.reserve0 as f64);
    let p_b = (pool_b.reserve1 as f64) / (pool_b.reserve0 as f64);
    let min_p = p_a.min(p_b);
    if min_p <= 0.0 {
        return 0;
    }

    let discrepancy = ((p_a - p_b).abs() / min_p) * 10_000.0;
    discrepancy as u128
}

fn optimal_input_from_spec(pool_buy: &PoolState, pool_sell: &PoolState) -> u128 {
    // Formula from spec.md exactly:
    // optimal_in = sqrt(r0_a * r0_b * r1_a / r1_b) - r0_a
    if pool_buy.reserve0 == 0
        || pool_buy.reserve1 == 0
        || pool_sell.reserve0 == 0
        || pool_sell.reserve1 == 0
    {
        return 0;
    }

    let inside = (pool_buy.reserve0 as f64)
        * (pool_sell.reserve0 as f64)
        * (pool_buy.reserve1 as f64)
        / (pool_sell.reserve1 as f64);

    if inside <= 0.0 {
        return 0;
    }

    let optimal = inside.sqrt() - (pool_buy.reserve0 as f64);
    if optimal <= 0.0 {
        return 0;
    }

    optimal as u128
}

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

/// Detects a two-pool V2 arbitrage opportunity.
///
/// Uses the exact formulas from `spec.md`:
/// - Optimal input: `sqrt(r0_a * r0_b * r1_a / r1_b) - r0_a`
/// - AMM output: `(amount_in * 997 * reserve_out) / (reserve_in * 1000 + amount_in * 997)`
///
/// Returns `None` when:
/// - Pools are incompatible
/// - Price discrepancy is <= 0.1%
/// - Estimated profit is below gas floor: `200_000 * base_fee`
pub fn detect_v2_arb_opportunity(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
) -> Option<ArbOpportunity> {
    if pool_a.token0 != pool_b.token0 || pool_a.token1 != pool_b.token1 {
        return None;
    }

    // Threshold: discrepancy > 0.1% = 10 bps.
    let discrepancy_bps = relative_discrepancy_bps(pool_a, pool_b);
    if discrepancy_bps <= 10 {
        return None;
    }

    let gas_floor = 200_000u128.saturating_mul(base_fee);

    // Evaluate both directions and keep the best profitable one.
    let input_ab = optimal_input_from_spec(pool_a, pool_b);
    let profit_ab = estimate_profit(input_ab, pool_a, pool_b);

    let input_ba = optimal_input_from_spec(pool_b, pool_a);
    let profit_ba = estimate_profit(input_ba, pool_b, pool_a);

    let (pool_1, pool_2, optimal_input_wei, profit_estimate_wei) = if profit_ab > profit_ba {
        (pool_a.address, pool_b.address, input_ab, profit_ab)
    } else if profit_ba > profit_ab {
        (pool_b.address, pool_a.address, input_ba, profit_ba)
    } else if input_ab >= input_ba {
        (pool_a.address, pool_b.address, input_ab, profit_ab)
    } else {
        (pool_b.address, pool_a.address, input_ba, profit_ba)
    };

    if optimal_input_wei == 0 || profit_estimate_wei < gas_floor {
        return None;
    }

    Some(ArbOpportunity {
        token_a: pool_a.token0,
        token_b: pool_a.token1,
        pool_1,
        pool_2,
        profit_estimate_wei,
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

        let decoded = match IUniswapV2Pair::getReservesCall::abi_decode_returns(&sim.output, true)
        {
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

    opportunities.sort_by(|lhs, rhs| rhs.profit_estimate_wei.cmp(&lhs.profit_estimate_wei));
    Ok(opportunities)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_pool(address: Address, reserve0: u128, reserve1: u128) -> PoolState {
        PoolState {
            address,
            token0: alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            token1: alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            reserve0,
            reserve1,
            fee_bps: 30,
        }
    }

    #[test]
    fn equal_prices_no_arb() {
        let pool_a = mk_pool(alloy::primitives::address!("1111111111111111111111111111111111111111"), 1_000_000, 2_000_000_000);
        let pool_b = mk_pool(alloy::primitives::address!("2222222222222222222222222222222222222222"), 500_000, 1_000_000_000);

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1);
        assert!(result.is_none());
    }

    #[test]
    fn half_percent_discrepancy_finds_arb() {
        // Pool A price: 2000 (2,000,000,000 / 1,000,000)
        // Pool B price: 2010 (2,010,000,000 / 1,000,000) = 0.5% discrepancy
        let pool_a = mk_pool(alloy::primitives::address!("3333333333333333333333333333333333333333"), 1_000_000, 2_000_000_000);
        let pool_b = mk_pool(alloy::primitives::address!("4444444444444444444444444444444444444444"), 1_000_000, 2_010_000_000);

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 0);
        assert!(result.is_some());

        let opportunity = result.expect("expected opportunity for 0.5% discrepancy");
        assert!(opportunity.optimal_input_wei > 0);
    }

    #[test]
    fn below_threshold_no_arb() {
        // ~0.05% discrepancy: should be below 0.1% threshold.
        let pool_a = mk_pool(alloy::primitives::address!("5555555555555555555555555555555555555555"), 1_000_000, 2_000_000_000);
        let pool_b = mk_pool(alloy::primitives::address!("6666666666666666666666666666666666666666"), 1_000_000, 2_001_000_000);

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1);
        assert!(result.is_none());
    }
}
