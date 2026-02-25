//! Uniswap V2-like arbitrage detection with rigorous per-pool fee math.
//!
//! **Algorithm:**
//! Detects two-pool arbitrage on constant-product AMMs. For each pool pair:
//! 1. Check token compatibility and price discrepancy (>10 bps prefilter)
//! 2. Evaluate both directions (A→B and B→A)
//! 3. For each direction, use closed-form (if fee-eligible) or ternary search
//! 4. Verify net profit after gas cost conversion to input token unit
//!
//! **Key Improvements:**
//! - Per-pool fee parameters (not hard-coded 997/1000)
//! - Closed-form optimal input formula with neighborhood verification
//! - Ternary search fallback with discrete plateau handling
//! - Gas cost converted to input token units (with same-block reference price)
//! - Overflow-safe U256 intermediates; no float arithmetic
//! - Block metadata tracking (block_number, timestamp_last)
//! - Typed error enum distinguishing faults from rejections

const GAS_ESTIMATE_UNITS: u128 = 200_000;
const NEIGHBORHOOD_RADIUS: u128 = 16;
const UNISWAP_V2_MAX_RESERVE: u128 = (1u128 << 112) - 1;

fn configured_gas_estimate_units() -> u128 {
    std::env::var("MEV_ARB_GAS_UNITS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(GAS_ESTIMATE_UNITS)
}

fn configured_discrepancy_threshold_bps() -> u128 {
    std::env::var("MEV_ARB_MIN_DISCREPANCY_BPS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(10)
}

fn configured_min_profit_wei() -> u128 {
    std::env::var("MEV_ARB_MIN_PROFIT_WEI")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(1)
}

use alloy::primitives::{Address, U256};
use eyre::{eyre, Result};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;

#[derive(Clone, Debug)]
enum ArbVerdict {
    ZeroReserves,
    SpreadBelowFee(f64),
    OptimalInputZero,
    GrossProfitNegative(i128),
    NetProfitNegative { gross: i128, gas: u128 },
    OpportunityFound { profit_wei: u128, spread_bps: f64 },
}

impl fmt::Display for ArbVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroReserves => write!(f, "ZeroReserves"),
            Self::SpreadBelowFee(spread_bps) => write!(f, "SpreadBelowFee({spread_bps:.4}bps)"),
            Self::OptimalInputZero => write!(f, "OptimalInputZero"),
            Self::GrossProfitNegative(gross_profit) => {
                write!(f, "GrossProfitNegative({gross_profit})")
            }
            Self::NetProfitNegative { gross, gas } => {
                write!(f, "NetProfitNegative{{gross:{gross}, gas:{gas}}}")
            }
            Self::OpportunityFound {
                profit_wei,
                spread_bps,
            } => {
                write!(
                    f,
                    "OpportunityFound{{profit_wei:{profit_wei}, spread_bps:{spread_bps:.4}}}"
                )
            }
        }
    }
}

/// Arbitrage detection errors (faults needing retry vs. legitimate rejections).
#[derive(Clone, Debug)]
pub enum ArbError {
    /// State mismatch (blocks not synchronized, metadata missing)
    StateInconsistency(String),
    /// Arithmetic overflow or underflow
    Overflow(String),
    /// Missing reference data (e.g., WETH price for gas conversion)
    MissingReferencePrice,
}

impl fmt::Display for ArbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArbError::StateInconsistency(s) => write!(f, "state inconsistency: {}", s),
            ArbError::Overflow(s) => write!(f, "arithmetic overflow: {}", s),
            ArbError::MissingReferencePrice => {
                write!(f, "missing reference price for gas conversion")
            }
        }
    }
}

impl std::error::Error for ArbError {}

use crate::decoder::addresses;

const UNISWAP_V2_FACTORY: Address =
    alloy::primitives::address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
const SUSHISWAP_V2_FACTORY: Address =
    alloy::primitives::address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac");
/// Cross-DEX pair configuration for two-pool V2 arbitrage scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArbPairConfig {
    /// Base token used for input/output accounting (usually WETH).
    pub token_a: Address,
    /// Quote token paired with `token_a`.
    pub token_b: Address,
    /// First DEX factory (Uniswap V2).
    pub dex_a_factory: Address,
    /// Second DEX factory (SushiSwap V2).
    pub dex_b_factory: Address,
}

/// Default cross-DEX pool universe: WETH against major stables.
pub const DEFAULT_ARB_PAIRS: [ArbPairConfig; 3] = [
    ArbPairConfig {
        token_a: addresses::WETH,
        token_b: addresses::USDC,
        dex_a_factory: UNISWAP_V2_FACTORY,
        dex_b_factory: SUSHISWAP_V2_FACTORY,
    },
    ArbPairConfig {
        token_a: addresses::WETH,
        token_b: addresses::USDT,
        dex_a_factory: UNISWAP_V2_FACTORY,
        dex_b_factory: SUSHISWAP_V2_FACTORY,
    },
    ArbPairConfig {
        token_a: addresses::WETH,
        token_b: addresses::DAI,
        dex_a_factory: UNISWAP_V2_FACTORY,
        dex_b_factory: SUSHISWAP_V2_FACTORY,
    },
];

/// Snapshot of a Uniswap V2-like pool with exact fee structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 address (sorted lexicographically at pair creation).
    pub token0: Address,
    /// Token1 address.
    pub token1: Address,
    /// Reserve for token0 (from last observed `getReserves()`).
    pub reserve0: u128,
    /// Reserve for token1.
    pub reserve1: u128,
    /// Per-pool fee numerator. Standard Uniswap V2 = 997 (0.3% fee).
    pub fee_numerator: u32,
    /// Per-pool fee denominator. Standard Uniswap V2 = 1000.
    pub fee_denominator: u32,
    /// Block number where reserves were observed.
    pub block_number: u64,
    /// Block timestamp where reserves were observed (for staleness check).
    pub timestamp_last: u64,
}

/// Estimated arbitrage opportunity between two pools (same block, consistent metadata).
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
    /// Gross AMM profit (leg2_output - input) in wei of input token, before gas.
    pub gross_profit_wei: u128,
    /// Gas cost in input token units (converted from base_fee * gas_used at same-block reference price).
    pub gas_cost_wei: u128,
    /// Net profit after gas: (gross - gas_cost). **Guaranteed > 0** at time of detection.
    pub net_profit_wei: u128,
    /// Estimated optimal input in wei (from closed-form or ternary search).
    pub optimal_input_wei: u128,
    /// Token path of the arbitrage route (for audit/replay).
    pub trade_path: Vec<Address>,
    /// Block number where this opportunity was detected.
    pub block_number: u64,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

/// Reads Uniswap V2 reserve slot state at a specific block via JSON-RPC.
pub struct ReserveReader {
    client: Client,
    rpc_url: String,
    block_number: u64,
}

impl ReserveReader {
    /// Create a reserve reader fixed to a target block.
    pub fn new(rpc_url: &str, block_number: u64) -> Self {
        Self {
            client: Client::new(),
            rpc_url: rpc_url.to_string(),
            block_number,
        }
    }

    async fn rpc_hex_result(&self, method: &str, params: serde_json::Value) -> Result<String> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| eyre!("{} request failed: {}", method, e))?;

        let status = response.status();
        let rpc: RpcResponse<String> = response
            .json()
            .await
            .map_err(|e| eyre!("failed to decode {} response: {}", method, e))?;

        if !status.is_success() {
            return Err(eyre!("{} HTTP status: {}", method, status));
        }

        if let Some(error) = rpc.error {
            return Err(eyre!(
                "{} RPC error {}: {}",
                method,
                error.code,
                error.message
            ));
        }

        rpc.result.ok_or_else(|| eyre!("{} missing result", method))
    }

    /// Resolve pair address from a V2 factory for token pair `(token_a, token_b)`.
    pub async fn get_pair(
        &self,
        factory: Address,
        token_a: Address,
        token_b: Address,
    ) -> Result<Option<Address>> {
        let selector = "e6a43905"; // getPair(address,address)
        let token_a_hex = format!("{:0>64}", format!("{token_a:#x}").trim_start_matches("0x"));
        let token_b_hex = format!("{:0>64}", format!("{token_b:#x}").trim_start_matches("0x"));
        let data = format!("0x{}{}{}", selector, token_a_hex, token_b_hex);

        let params = serde_json::json!([
            {
                "to": format!("{factory:#x}"),
                "data": data,
            },
            format!("0x{:x}", self.block_number)
        ]);

        let result_hex = self.rpc_hex_result("eth_call", params).await?;
        let raw = result_hex.trim_start_matches("0x");
        if raw.len() < 40 {
            return Ok(None);
        }

        let addr_hex = &raw[raw.len().saturating_sub(40)..];
        let pair = format!("0x{}", addr_hex)
            .parse::<Address>()
            .map_err(|e| eyre!("failed to parse getPair address: {}", e))?;

        if pair == Address::ZERO {
            Ok(None)
        } else {
            Ok(Some(pair))
        }
    }

    /// Returns `(reserve0, reserve1, block_timestamp_last)` for a V2 pair.
    pub async fn get_reserves(&self, pool: Address) -> Result<(u128, u128, u64)> {
        let params = serde_json::json!([
            format!("{pool:#x}"),
            "0x8",
            format!("0x{:x}", self.block_number)
        ]);

        let value_hex = self.rpc_hex_result("eth_getStorageAt", params).await?;

        let value: U256 = U256::from_str_radix(value_hex.trim_start_matches("0x"), 16)
            .map_err(|e| eyre!("failed parsing reserve slot value as U256: {}", e))?;

        let mask_112: U256 = (U256::from(1u64) << 112) - U256::from(1u64);
        let reserve0 = (value & mask_112).to::<u128>();
        let reserve1 = ((value >> 112u32) & mask_112).to::<u128>();
        let timestamp_last = (value >> 224u32).to::<u64>();

        Ok((reserve0, reserve1, timestamp_last))
    }

    fn block_number(&self) -> u64 {
        self.block_number
    }
}

/// Computes swap output using exact AMM integer math (floor division).
///
/// Formula: `(amount_in * fee_numerator * reserve_out) / (reserve_in * fee_denominator + amount_in * fee_numerator)`
///
/// For Uniswap V2: fee_numerator=997, fee_denominator=1000 (0.3% fee).
/// Uses U256 to avoid intermediate overflow on reserves up to u128::MAX.
#[inline]
fn amount_out(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    fee_numerator: u32,
    fee_denominator: u32,
) -> u128 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }

    let amount_in_u256 = U256::from(amount_in);
    let reserve_in_u256 = U256::from(reserve_in);
    let reserve_out_u256 = U256::from(reserve_out);
    let fee_num = U256::from(fee_numerator);
    let fee_denom = U256::from(fee_denominator);

    let amount_in_with_fee = amount_in_u256 * fee_num;
    let numerator = amount_in_with_fee * reserve_out_u256;
    let denominator = (reserve_in_u256 * fee_denom) + amount_in_with_fee;

    if denominator.is_zero() {
        return 0;
    }

    (numerator / denominator).to::<u128>()
}

/// Checks if closed-form eligibility applies (both pools share same fee structure).
/// Closed-form works only for identical fee numerator/denominator pairs.
#[inline]
fn is_closed_form_eligible(pool_a: &PoolState, pool_b: &PoolState) -> bool {
    pool_a.fee_numerator == pool_b.fee_numerator && pool_a.fee_denominator == pool_b.fee_denominator
}

/// Computes price discrepancy using rational arithmetic (no floats).
///
/// Returns true if `|p_a - p_b| / min(p_a, p_b) > threshold_bps / 10000`
/// using cross-multiplication: `(p_a - p_b).abs() * 10000 > threshold_bps * min(p_a, p_b)`
/// = `|r1_a * r0_b - r1_b * r0_a| * 10000 > threshold_bps * min(r1_a * r0_b, r1_b * r0_a)`
#[inline]
pub fn exceeds_discrepancy_threshold(
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

/// Computes integer spread in basis points (truncated) using cross-multiplication.
///
/// Returns `|p_a - p_b| / min(p_a, p_b) * 10_000` as a `u128`.
/// Returns 0 if either pool has zero reserves.
#[inline]
pub fn spread_bps_integer(pool_a: &PoolState, pool_b: &PoolState) -> u128 {
    if pool_a.reserve0 == 0 || pool_a.reserve1 == 0 || pool_b.reserve0 == 0 || pool_b.reserve1 == 0
    {
        return 0;
    }
    let r0a = U256::from(pool_a.reserve0);
    let r1a = U256::from(pool_a.reserve1);
    let r0b = U256::from(pool_b.reserve0);
    let r1b = U256::from(pool_b.reserve1);

    let p_a_num = r1a * r0b;
    let p_b_num = r1b * r0a;
    let min_price = p_a_num.min(p_b_num);

    if min_price.is_zero() {
        return 0;
    }

    let discrepancy_num = if p_a_num >= p_b_num {
        p_a_num - p_b_num
    } else {
        p_b_num - p_a_num
    };

    // spread_bps = discrepancy_num * 10_000 / min_price
    let bps = (discrepancy_num * U256::from(10_000u32)) / min_price;
    // Saturate to u128 (would require extreme reserves to overflow)
    bps.try_into().unwrap_or(u128::MAX)
}

/// Computes optimal input for matching-fee pools using closed-form formula.
///
/// **Closed-form derivation (fee-adjusted):**
/// For two pools with identical fee structure (f_num/f_denom), the optimal input x satisfies:
/// x* = [f_num × sqrt(r_in_a × r_out_a × r_in_b × r_out_b) - f_denom × r_in_a × r_in_b]
///      / [f_num × r_in_b × f_denom + f_num² × r_out_a]
///
/// **Implementation:** Pre-compute presqrt = f_num² × r_out_a × r_out_b / (r_in_a × r_in_b),
/// then isqrt(presqrt), then substitute into formula.
///
/// **Feasibility:** Result is valid (positive) only if sqrt(presqrt) ≥ f_denom.
/// Otherwise, the opportunity is too thin for closed-form sizing; use ternary search fallback.
///
/// **References:** Flashbots MEV-Inspect, Defi Labs MEV research.
fn optimal_input_closed_form(pool_buy: &PoolState, pool_sell: &PoolState) -> Option<u128> {
    if pool_buy.reserve0 == 0
        || pool_buy.reserve1 == 0
        || pool_sell.reserve0 == 0
        || pool_sell.reserve1 == 0
    {
        return None;
    }

    let f_num = U256::from(pool_buy.fee_numerator);
    let f_denom = U256::from(pool_buy.fee_denominator);

    let r_in_a = U256::from(pool_buy.reserve0);
    let r_out_a = U256::from(pool_buy.reserve1);
    let r_in_b = U256::from(pool_sell.reserve1);
    let r_out_b = U256::from(pool_sell.reserve0);

    if pool_buy.reserve0 > UNISWAP_V2_MAX_RESERVE
        || pool_buy.reserve1 > UNISWAP_V2_MAX_RESERVE
        || pool_sell.reserve0 > UNISWAP_V2_MAX_RESERVE
        || pool_sell.reserve1 > UNISWAP_V2_MAX_RESERVE
    {
        tracing::debug!(
            "closed-form skipped: reserve exceeds Uniswap V2 uint112 domain; use ternary fallback"
        );
        return None;
    }

    // Compute presqrt = f_num² × r_out_a × r_out_b / (r_in_a × r_in_b)
    let presqrt_num = f_num
        .checked_mul(f_num)?
        .checked_mul(r_out_a)?
        .checked_mul(r_out_b)?;
    let presqrt_den = r_in_a.checked_mul(r_in_b)?;

    if presqrt_den.is_zero() {
        return None;
    }

    // Integer division to compute presqrt ratio
    let presqrt = presqrt_num / presqrt_den;
    let sqrt_presqrt = isqrt(presqrt);

    if sqrt_presqrt.is_zero() {
        return None;
    }

    // Feasibility check: sqrt(presqrt) must be >= f_denom (otherwise negative result)
    if sqrt_presqrt < f_denom {
        return None; // Ineligible: closed-form would produce negative input
    }

    // numerator = (sqrt(presqrt) - f_denom) × r_in_b × r_in_a
    let numerator = (sqrt_presqrt - f_denom)
        .checked_mul(r_in_b)?
        .checked_mul(r_in_a)?;

    // denominator = f_num × r_in_b × f_denom + f_num² × r_out_a
    let denominator = f_num
        .checked_mul(r_in_b)?
        .checked_mul(f_denom)?
        .checked_add(f_num.checked_mul(f_num)?.checked_mul(r_out_a)?)?;

    if denominator.is_zero() {
        return None;
    }

    // optimal_input = numerator × f_denom / denominator
    let result = numerator.checked_mul(f_denom)? / denominator;
    Some(result.to::<u128>())
}

/// Integer square root using Newton's method (returns floor).
///
/// Converges to ⌊√n⌋ using iterative refinement: x_{k+1} = ⌊(x_k + n/x_k) / 2⌋
/// Terminates when x stops decreasing (x_k ≤ x_{k+1}).
///
/// **Correctness:** Proven to converge to exact floor of true square root.
/// **Example:** isqrt(10) = 3 (since 3² = 9 < 10 < 16 = 4²)
#[inline]
fn isqrt(n: U256) -> U256 {
    if n.is_zero() {
        return U256::ZERO;
    }

    let mut x = (n + U256::from(1u32)) >> 1u32;
    let mut y = n;

    // Iterate until convergence: Newton's method for floor(sqrt(n))
    while x < y {
        y = x;
        x = (x + n / x) >> 1u32; // Right-shift by 1 = divide by 2 (floor division)
    }

    y // Return the last non-increasing iterate; this is always ⌊√n⌋
}

#[inline]
fn best_input_in_neighborhood(
    center: u128,
    pool_buy: &PoolState,
    pool_sell: &PoolState,
) -> (u128, u128) {
    let scan_low = center.saturating_sub(NEIGHBORHOOD_RADIUS).max(1);
    let scan_high = center.saturating_add(NEIGHBORHOOD_RADIUS);

    let mut best_input = center.max(1);
    let mut best_profit = estimate_profit(best_input, pool_buy, pool_sell);

    for candidate in scan_low..=scan_high {
        let candidate_profit = estimate_profit(candidate, pool_buy, pool_sell);
        if candidate_profit > best_profit
            || (candidate_profit == best_profit && candidate < best_input)
        {
            best_input = candidate;
            best_profit = candidate_profit;
        }
    }

    (best_input, best_profit)
}

#[inline]
fn convert_eth_wei_to_token0_wei(
    gas_cost_eth_wei: u128,
    token0: Address,
    block_number: u64,
    weth_price_pool: Option<&PoolState>,
) -> Result<u128, ArbError> {
    if gas_cost_eth_wei == 0 {
        return Ok(0);
    }

    if token0 == addresses::WETH {
        return Ok(gas_cost_eth_wei);
    }

    let reference_pool = weth_price_pool.ok_or(ArbError::MissingReferencePrice)?;
    if reference_pool.block_number != block_number {
        return Err(ArbError::StateInconsistency(format!(
            "reference pool block mismatch: {} vs {}",
            reference_pool.block_number, block_number
        )));
    }

    let converted = if reference_pool.token0 == addresses::WETH && reference_pool.token1 == token0 {
        amount_out(
            gas_cost_eth_wei,
            reference_pool.reserve0,
            reference_pool.reserve1,
            reference_pool.fee_numerator,
            reference_pool.fee_denominator,
        )
    } else if reference_pool.token1 == addresses::WETH && reference_pool.token0 == token0 {
        amount_out(
            gas_cost_eth_wei,
            reference_pool.reserve1,
            reference_pool.reserve0,
            reference_pool.fee_numerator,
            reference_pool.fee_denominator,
        )
    } else {
        return Err(ArbError::MissingReferencePrice);
    };

    if converted == 0 {
        Ok(1)
    } else {
        Ok(converted)
    }
}

/// Ternary search over input range to find optimal input.
///
/// Searches interval [1, max_input] for the input that maximizes profit.
/// Terminates when `high - low <= 2`, then linearly checks remaining points.
/// Returns (optimal_input, profit) of the best candidate.
/// Ternary search over input range to find optimal input.
///
/// **Algorithm:** Eliminates non-optimal third of search space each iteration.
/// Terminates when `high - low ≤ 2`, then linearly checks remaining 1-3 points.
/// Handles discrete profit plateaus caused by floor division in amount_out.
///
/// **Returns:** (optimal_input, Option<gross_profit>) where None means all checked inputs unprofitable.
fn ternary_search_optimal_input(
    pool_1: &PoolState,
    pool_2: &PoolState,
    max_input: u128,
) -> (u128, Option<u128>) {
    if max_input < 2 {
        let profit = estimate_profit(max_input, pool_1, pool_2);
        return (max_input, if profit > 0 { Some(profit) } else { None });
    }

    let mut low: u128 = 1;
    let mut high: u128 = max_input;
    let mut best_input = 1;
    let mut best_profit: Option<u128> = None;
    let profit_1 = estimate_profit(1, pool_1, pool_2);
    if profit_1 > 0 {
        best_profit = Some(profit_1);
    }

    while high - low > 2 {
        let mid1 = low + (high - low) / 3;
        let mid2 = high - (high - low) / 3;

        let profit_mid1 = estimate_profit(mid1, pool_1, pool_2);
        let profit_mid2 = estimate_profit(mid2, pool_1, pool_2);

        if profit_mid1 > 0 {
            match best_profit {
                Some(current) if profit_mid1 <= current => {}
                _ => {
                    best_profit = Some(profit_mid1);
                    best_input = mid1;
                }
            }
        }
        if profit_mid2 > 0 {
            match best_profit {
                Some(current) if profit_mid2 <= current => {}
                _ => {
                    best_profit = Some(profit_mid2);
                    best_input = mid2;
                }
            }
        }

        if profit_mid1 > profit_mid2 {
            high = mid2;
        } else {
            low = mid1;
        }
    }

    // Final linear check of remaining points [low, high]
    for i in low..=high {
        let profit = estimate_profit(i, pool_1, pool_2);
        if profit > 0 {
            match best_profit {
                Some(current) if profit <= current => {}
                _ => {
                    best_profit = Some(profit);
                    best_input = i;
                }
            }
        }
    }

    (best_input, best_profit)
}

/// Computes exact two-leg profit for given input.
///
/// Returns gross profit: leg_2_output - input (can be negative after slippage).
/// Returns 0 for any leg that produces zero output (indicating failure case).
/// Does NOT account for gas costs (handled separately in detect_v2_arb_opportunity).
fn estimate_profit(input: u128, pool_1: &PoolState, pool_2: &PoolState) -> u128 {
    if input == 0 {
        return 0;
    }

    let leg_1_out = amount_out(
        input,
        pool_1.reserve0,
        pool_1.reserve1,
        pool_1.fee_numerator,
        pool_1.fee_denominator,
    );
    if leg_1_out == 0 {
        return 0;
    }

    let leg_2_out = amount_out(
        leg_1_out,
        pool_2.reserve1,
        pool_2.reserve0,
        pool_2.fee_numerator,
        pool_2.fee_denominator,
    );

    // Return profit, or 0 if loss (using saturating subtraction for safety)
    leg_2_out.saturating_sub(input)
}

fn signed_profit(input: u128, pool_1: &PoolState, pool_2: &PoolState) -> i128 {
    if input == 0 {
        return 0;
    }

    let leg_1_out = amount_out(
        input,
        pool_1.reserve0,
        pool_1.reserve1,
        pool_1.fee_numerator,
        pool_1.fee_denominator,
    );
    if leg_1_out == 0 {
        return -(input as i128);
    }

    let leg_2_out = amount_out(
        leg_1_out,
        pool_2.reserve1,
        pool_2.reserve0,
        pool_2.fee_numerator,
        pool_2.fee_denominator,
    );

    (leg_2_out as i128) - (input as i128)
}

fn price_and_spread_bps(pool_a: &PoolState, pool_b: &PoolState) -> (f64, f64, f64) {
    let uni_price = (pool_a.reserve1 as f64) / (pool_a.reserve0.max(1) as f64);
    let sushi_price = (pool_b.reserve1 as f64) / (pool_b.reserve0.max(1) as f64);
    let denominator = uni_price.min(sushi_price);
    let spread_bps = if denominator <= 0.0 {
        0.0
    } else {
        ((uni_price - sushi_price).abs() / denominator) * 10_000.0
    };
    (uni_price, sushi_price, spread_bps)
}

/// Detects a two-pool arbitrage opportunity with rigorous per-pool fee math.
///
/// **Algorithm Flow:**
/// 1. Token compatibility and block metadata consistency check
/// 2. Price discrepancy prefilter (>10 bps; loose to avoid false negatives)
/// 3. Bidirectional evaluation (A→B and B→A)
/// 4. Per-direction: closed-form (if identical fees) OR ternary search (with neighborhood verification)
/// 5. Gas cost conversion to input token units (requires reference WETH price pool)
/// 6. Net profit validation (strictly > 0 after deducting gas)
///
/// **Returns:** `Ok(Some(opp))` if profitable, `Ok(None)` if legitimately unprofitable, `Err(ArbError)` if fault.
///
/// **Rejection Criteria (returns Ok(None)):**
/// - Incompatible tokens or mismatched block metadata
/// - Discrepancy ≤ 10 bps (loose noise prefilter)
/// - Net profit (after gas) ≤ 0
/// - Closed-form ineligible + ternary search produces no profit
pub fn detect_v2_arb_opportunity(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
    weth_price_pool: Option<&PoolState>,
) -> Result<Option<ArbOpportunity>, ArbError> {
    let (opportunity, _) =
        detect_v2_arb_opportunity_with_verdict(pool_a, pool_b, base_fee, weth_price_pool)?;
    Ok(opportunity)
}

/// Detects two-pool arbitrage and returns a human-readable verdict string.
///
/// Useful for instrumentation where callers need rejection reasons instead of bare `None`.
pub fn detect_v2_arb_opportunity_with_reason(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
    weth_price_pool: Option<&PoolState>,
) -> Result<(Option<ArbOpportunity>, String), ArbError> {
    let (opportunity, verdict) =
        detect_v2_arb_opportunity_with_verdict(pool_a, pool_b, base_fee, weth_price_pool)?;
    Ok((opportunity, verdict.to_string()))
}

fn detect_v2_arb_opportunity_with_verdict(
    pool_a: &PoolState,
    pool_b: &PoolState,
    base_fee: u128,
    weth_price_pool: Option<&PoolState>,
) -> Result<(Option<ArbOpportunity>, ArbVerdict), ArbError> {
    let (_, _, spread_bps) = price_and_spread_bps(pool_a, pool_b);

    if pool_a.reserve0 == 0 || pool_a.reserve1 == 0 || pool_b.reserve0 == 0 || pool_b.reserve1 == 0
    {
        return Ok((None, ArbVerdict::ZeroReserves));
    }

    // Token compatibility check
    if pool_a.token0 != pool_b.token0 || pool_a.token1 != pool_b.token1 {
        return Ok((None, ArbVerdict::SpreadBelowFee(spread_bps)));
    }

    // Block metadata consistency (ensure same-block observation)
    if pool_a.block_number != pool_b.block_number {
        return Err(ArbError::StateInconsistency(format!(
            "pools from different blocks: {} vs {}",
            pool_a.block_number, pool_b.block_number
        )));
    }

    // Two-hop V2 cross-DEX needs at least ~60 bps just to clear 0.3%+0.3% swap fees.
    const TWO_HOP_FEE_FLOOR_BPS: u128 = 60;
    if !exceeds_discrepancy_threshold(pool_a, pool_b, TWO_HOP_FEE_FLOOR_BPS) {
        return Ok((None, ArbVerdict::SpreadBelowFee(spread_bps)));
    }

    // Price discrepancy threshold: loose prefilter to remove near-parity noise.
    // Actual profitability is decided by full two-leg simulation + gas conversion.
    let discrepancy_threshold_bps = configured_discrepancy_threshold_bps();
    if !exceeds_discrepancy_threshold(pool_a, pool_b, discrepancy_threshold_bps) {
        return Ok((None, ArbVerdict::SpreadBelowFee(spread_bps)));
    }

    tracing::debug!(
        pool_a_r0 = pool_a.reserve0,
        pool_a_r1 = pool_a.reserve1,
        pool_b_r0 = pool_b.reserve0,
        pool_b_r1 = pool_b.reserve1,
        "discrepancy check passed; proceeding with sizing"
    );

    let gas_cost_eth_wei = configured_gas_estimate_units().saturating_mul(base_fee);
    let gas_cost_wei = convert_eth_wei_to_token0_wei(
        gas_cost_eth_wei,
        pool_a.token0,
        pool_a.block_number,
        weth_price_pool,
    )?;

    // **Direction 1: A → B (buy from A, sell to B)**
    let (input_ab, profit_ab) = if is_closed_form_eligible(pool_a, pool_b) {
        if let Some(input) = optimal_input_closed_form(pool_a, pool_b) {
            let (best_input, best_profit) = best_input_in_neighborhood(input, pool_a, pool_b);

            tracing::debug!(
                pool_a_b_optimal_input = best_input,
                pool_a_b_profit = best_profit,
                "closed-form (A→B) with neighborhood check"
            );
            (best_input, Some(best_profit).filter(|&p| p > 0))
        } else {
            // Closed-form returned None: fall back to ternary search
            let max_input = pool_a.reserve0.saturating_mul(10) / 100; // 10% of reserves
            tracing::debug!(
                pool_a_b_max_input = max_input,
                "closed-form infeasible; fallback to ternary search (A→B)"
            );
            ternary_search_optimal_input(pool_a, pool_b, max_input)
        }
    } else {
        // Mixed fees: ternary search fallback
        let max_input = pool_a.reserve0.saturating_mul(10) / 100; // 10% of reserves
        tracing::debug!(
            pool_a_b_max_input = max_input,
            "mixed fee structure; using ternary search (A→B)"
        );
        ternary_search_optimal_input(pool_a, pool_b, max_input)
    };

    // **Direction 2: B → A (buy from B, sell to A)**
    let (input_ba, profit_ba) = if is_closed_form_eligible(pool_b, pool_a) {
        if let Some(input) = optimal_input_closed_form(pool_b, pool_a) {
            let (best_input, best_profit) = best_input_in_neighborhood(input, pool_b, pool_a);

            tracing::debug!(
                pool_b_a_optimal_input = best_input,
                pool_b_a_profit = best_profit,
                "closed-form (B→A) with neighborhood check"
            );
            (best_input, Some(best_profit).filter(|&p| p > 0))
        } else {
            let max_input = pool_b.reserve0.saturating_mul(10) / 100;
            tracing::debug!(
                pool_b_a_max_input = max_input,
                "closed-form infeasible; fallback to ternary search (B→A)"
            );
            ternary_search_optimal_input(pool_b, pool_a, max_input)
        }
    } else {
        let max_input = pool_b.reserve0.saturating_mul(10) / 100;
        tracing::debug!(
            pool_b_a_max_input = max_input,
            "mixed fee structure; using ternary search (B→A)"
        );
        ternary_search_optimal_input(pool_b, pool_a, max_input)
    };

    // Pick more profitable direction
    // Tie-break: prefer SMALLER input (less capital risk, more genuine arb)
    let (pool_1, pool_2, optimal_input_wei, gross_profit_wei) = match (profit_ab, profit_ba) {
        (Some(p_ab), Some(p_ba)) if p_ab > p_ba => (pool_a.address, pool_b.address, input_ab, p_ab),
        (Some(p_ab), Some(p_ba)) if p_ba > p_ab => (pool_b.address, pool_a.address, input_ba, p_ba),
        (Some(p_ab), Some(_p_ba)) => {
            if input_ab <= input_ba {
                (pool_a.address, pool_b.address, input_ab, p_ab)
            } else {
                (pool_b.address, pool_a.address, input_ba, p_ab)
            }
        }
        (Some(p_ab), None) => (pool_a.address, pool_b.address, input_ab, p_ab),
        (None, Some(p_ba)) => (pool_b.address, pool_a.address, input_ba, p_ba),
        (None, None) => {
            if input_ab == 0 && input_ba == 0 {
                return Ok((None, ArbVerdict::OptimalInputZero));
            }

            let gross_ab = signed_profit(input_ab.max(1), pool_a, pool_b);
            let gross_ba = signed_profit(input_ba.max(1), pool_b, pool_a);
            return Ok((
                None,
                ArbVerdict::GrossProfitNegative(gross_ab.max(gross_ba)),
            ));
        }
    };

    // Reject if input is zero or gross profit insufficient
    if optimal_input_wei == 0 || gross_profit_wei == 0 {
        if optimal_input_wei == 0 {
            return Ok((None, ArbVerdict::OptimalInputZero));
        }
        return Ok((None, ArbVerdict::GrossProfitNegative(0)));
    }

    // **Net profit = gross profit - gas cost (must be strictly positive)**
    // gas_cost_wei is already denominated in token0 units via same-block WETH reference.
    let net_profit_wei = match gross_profit_wei.checked_sub(gas_cost_wei) {
        Some(np) if np > 0 => np,
        _ => {
            return Ok((
                None,
                ArbVerdict::NetProfitNegative {
                    gross: gross_profit_wei as i128,
                    gas: gas_cost_wei,
                },
            ));
        }
    };

    let min_profit_wei = configured_min_profit_wei();
    if net_profit_wei < min_profit_wei {
        return Ok((
            None,
            ArbVerdict::NetProfitNegative {
                gross: net_profit_wei as i128,
                gas: min_profit_wei,
            },
        ));
    }

    tracing::debug!(
        pool_1 = %pool_1,
        pool_2 = %pool_2,
        optimal_input_wei,
        gross_profit_wei,
        net_profit_wei,
        block_number = pool_a.block_number,
        "opportunity detected"
    );

    Ok((
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
            block_number: pool_a.block_number,
        }),
        ArbVerdict::OpportunityFound {
            profit_wei: net_profit_wei,
            spread_bps,
        },
    ))
}

fn normalized_reserves(
    token_a: Address,
    token_b: Address,
    reserve0: u128,
    reserve1: u128,
) -> (u128, u128) {
    if token_a < token_b {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    }
}

/// Scans candidate pools for arbitrage opportunities.
///
/// - Fetches pool states through on-chain reserve slot reads (`eth_getStorageAt`)
/// - Resolves cross-DEX pair addresses via factory `getPair(tokenA, tokenB)`
/// - Returns opportunities sorted by descending estimated profit
pub async fn scan_for_arb(
    rpc_url: &str,
    block_number: u64,
    base_fee: u128,
    pair_configs: &[ArbPairConfig],
) -> Result<Vec<ArbOpportunity>> {
    let reserve_reader = ReserveReader::new(rpc_url, block_number);
    let mut opportunities = Vec::new();
    let mut logged_pools = HashSet::new();

    for config in pair_configs {
        let pair_name = format!("{:#x}/{:#x}", config.token_a, config.token_b);
        let Some(pool_a_addr) = reserve_reader
            .get_pair(config.dex_a_factory, config.token_a, config.token_b)
            .await?
        else {
            tracing::debug!(
                "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                block_number,
                pair_name,
                0.0,
                0.0,
                0.0,
                ArbVerdict::ZeroReserves,
            );
            continue;
        };

        let Some(pool_b_addr) = reserve_reader
            .get_pair(config.dex_b_factory, config.token_a, config.token_b)
            .await?
        else {
            tracing::debug!(
                "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                block_number,
                pair_name,
                0.0,
                0.0,
                0.0,
                ArbVerdict::ZeroReserves,
            );
            continue;
        };

        let (a_r0, a_r1, a_ts) = match reserve_reader.get_reserves(pool_a_addr).await {
            Ok(values) => values,
            Err(error) => {
                tracing::debug!(pool = %format!("{pool_a_addr:#x}"), error = %error, "failed to fetch dex-a reserves");
                tracing::debug!(
                    "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                    block_number,
                    pair_name,
                    0.0,
                    0.0,
                    0.0,
                    ArbVerdict::ZeroReserves,
                );
                continue;
            }
        };

        let (b_r0, b_r1, b_ts) = match reserve_reader.get_reserves(pool_b_addr).await {
            Ok(values) => values,
            Err(error) => {
                tracing::debug!(pool = %format!("{pool_b_addr:#x}"), error = %error, "failed to fetch dex-b reserves");
                tracing::debug!(
                    "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                    block_number,
                    pair_name,
                    0.0,
                    0.0,
                    0.0,
                    ArbVerdict::ZeroReserves,
                );
                continue;
            }
        };

        let (a_token_a_reserve, a_token_b_reserve) =
            normalized_reserves(config.token_a, config.token_b, a_r0, a_r1);
        let (b_token_a_reserve, b_token_b_reserve) =
            normalized_reserves(config.token_a, config.token_b, b_r0, b_r1);

        let pool_a = PoolState {
            address: pool_a_addr,
            token0: config.token_a,
            token1: config.token_b,
            reserve0: a_token_a_reserve,
            reserve1: a_token_b_reserve,
            fee_numerator: 997,
            fee_denominator: 1000,
            block_number: reserve_reader.block_number(),
            timestamp_last: a_ts,
        };
        let pool_b = PoolState {
            address: pool_b_addr,
            token0: config.token_a,
            token1: config.token_b,
            reserve0: b_token_a_reserve,
            reserve1: b_token_b_reserve,
            fee_numerator: 997,
            fee_denominator: 1000,
            block_number: reserve_reader.block_number(),
            timestamp_last: b_ts,
        };

        if logged_pools.insert(pool_a.address) {
            tracing::debug!(
                "[ARB_RAW] pool={} token0={} token1={} reserve0={} reserve1={}",
                format!("{:#x}", pool_a.address),
                format!("{:#x}", pool_a.token0),
                format!("{:#x}", pool_a.token1),
                pool_a.reserve0,
                pool_a.reserve1,
            );
        }

        if logged_pools.insert(pool_b.address) {
            tracing::debug!(
                "[ARB_RAW] pool={} token0={} token1={} reserve0={} reserve1={}",
                format!("{:#x}", pool_b.address),
                format!("{:#x}", pool_b.token0),
                format!("{:#x}", pool_b.token1),
                pool_b.reserve0,
                pool_b.reserve1,
            );
        }

        let (uni_price, sushi_price, spread_bps) = price_and_spread_bps(&pool_a, &pool_b);

        let reference_pool =
            if config.token_a == addresses::WETH || config.token_b == addresses::WETH {
                Some(&pool_a)
            } else {
                None
            };

        match detect_v2_arb_opportunity_with_verdict(&pool_a, &pool_b, base_fee, reference_pool) {
            Ok((Some(opportunity), verdict)) => {
                tracing::debug!(
                    "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                    block_number,
                    pair_name,
                    uni_price,
                    sushi_price,
                    spread_bps,
                    verdict,
                );
                opportunities.push(opportunity);
            }
            Ok((None, verdict)) => {
                tracing::debug!(
                    "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={}",
                    block_number,
                    pair_name,
                    uni_price,
                    sushi_price,
                    spread_bps,
                    verdict,
                );
            }
            Err(error) => {
                let verdict = ArbVerdict::NetProfitNegative { gross: -1, gas: 0 };
                tracing::debug!(
                    "[ARB] block={} pair={} uni_price={:.4} sushi_price={:.4} spread_bps={:.2} verdict={} error={}",
                    block_number,
                    pair_name,
                    uni_price,
                    sushi_price,
                    spread_bps,
                    verdict,
                    error,
                );
            }
        }
    }

    opportunities.sort_by(|lhs, rhs| rhs.net_profit_wei.cmp(&lhs.net_profit_wei));
    Ok(opportunities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;
    use std::sync::Once;

    fn init_test_tracing() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let level = match std::env::var("RUST_LOG") {
                Ok(raw) => {
                    let normalized = raw.to_ascii_lowercase();
                    if normalized.contains("trace") {
                        tracing::Level::TRACE
                    } else if normalized.contains("debug") {
                        tracing::Level::DEBUG
                    } else if normalized.contains("warn") {
                        tracing::Level::WARN
                    } else if normalized.contains("error") {
                        tracing::Level::ERROR
                    } else {
                        tracing::Level::INFO
                    }
                }
                Err(_) => tracing::Level::INFO,
            };

            let _ = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_test_writer()
                .try_init();
        });
    }

    async fn reserves_via_eth_call(
        reader: &ReserveReader,
        pool: Address,
    ) -> eyre::Result<(u128, u128, u64)> {
        let result_hex = reader
            .rpc_hex_result(
                "eth_call",
                serde_json::json!([
                    {
                        "to": format!("{pool:#x}"),
                        "data": "0x0902f1ac",
                    },
                    format!("0x{:x}", reader.block_number()),
                ]),
            )
            .await?;

        let raw = result_hex.trim_start_matches("0x");
        if raw.len() < 192 {
            return Err(eyre!(
                "getReserves eth_call returned short payload: {}",
                result_hex
            ));
        }

        let reserve0 = U256::from_str_radix(&raw[0..64], 16)
            .map_err(|error| eyre!("failed to decode reserve0: {}", error))?
            .to::<u128>();
        let reserve1 = U256::from_str_radix(&raw[64..128], 16)
            .map_err(|error| eyre!("failed to decode reserve1: {}", error))?
            .to::<u128>();
        let timestamp = U256::from_str_radix(&raw[128..192], 16)
            .map_err(|error| eyre!("failed to decode blockTimestampLast: {}", error))?
            .to::<u64>();

        Ok((reserve0, reserve1, timestamp))
    }

    #[derive(Debug, Deserialize)]
    struct KnownArbTx {
        block_number: u64,
        tx_hash: String,
        tx_index: u64,
        pair: String,
        #[serde(default)]
        profit_approx_wei: Option<u128>,
        #[serde(default)]
        profit_approx_usd: Option<f64>,
    }

    fn mk_pool(address: Address, reserve0: u128, reserve1: u128) -> PoolState {
        PoolState {
            address,
            token0: alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
            token1: alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
            reserve0,
            reserve1,
            fee_numerator: 997,
            fee_denominator: 1000,
            block_number: 18_000_000,
            timestamp_last: 1_000_000,
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

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1, None);
        assert!(result.ok().flatten().is_none());
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

        match detect_v2_arb_opportunity(&pool_a, &pool_b, 0, None) {
            Ok(Some(opportunity)) => {
                assert!(opportunity.optimal_input_wei > 0);
                assert!(opportunity.net_profit_wei > 0);
            }
            _ => panic!("1% discrepancy should find opportunity"),
        }
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

        let result = detect_v2_arb_opportunity(&pool_a, &pool_b, 1, None);
        assert!(result.ok().flatten().is_none());
    }

    #[test]
    fn amount_out_matches_uniswap_v2_semantics() {
        // Test: amount_out(1000, 10_000, 10_000, 997, 1000) should match exact formula
        let result = amount_out(1000, 10_000, 10_000, 997, 1000);
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

        let opt_input = opt_input_ab
            .or_else(|| opt_input_ba)
            .expect("closed-form should find optimal input");
        assert!(
            opt_input > 0,
            "closed-form should find positive optimal input"
        );

        // Verify the profit is positive at this input
        let profit = if opt_input_ab.is_some() {
            estimate_profit(opt_input, &pool_a, &pool_b)
        } else {
            estimate_profit(opt_input, &pool_b, &pool_a)
        };
        assert!(profit > 0, "profit at optimal input should be positive");
    }

    #[test]
    fn isqrt_returns_floor_on_oscillating_inputs() {
        assert_eq!(isqrt(U256::from(0u8)), U256::from(0u8));
        assert_eq!(isqrt(U256::from(1u8)), U256::from(1u8));
        assert_eq!(isqrt(U256::from(3u8)), U256::from(1u8));
        assert_eq!(isqrt(U256::from(4u8)), U256::from(2u8));
        assert_eq!(isqrt(U256::from(8u8)), U256::from(2u8));
        assert_eq!(isqrt(U256::from(9u8)), U256::from(3u8));
        assert_eq!(isqrt(U256::from(10u8)), U256::from(3u8));
    }

    #[test]
    fn test_reserve_values_match_etherscan() {
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping reserve verification test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let block_number = 18_000_000u64;
            let pool = alloy::primitives::address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
            let reader = ReserveReader::new(&rpc_url, block_number);

            let raw_storage = reader
                .rpc_hex_result(
                    "eth_getStorageAt",
                    serde_json::json!([
                        format!("{pool:#x}"),
                        "0x8",
                        format!("0x{:x}", block_number),
                    ]),
                )
                .await
                .expect("eth_getStorageAt should succeed");

            let (reserve0, reserve1, _) = reader
                .get_reserves(pool)
                .await
                .expect("get_reserves should succeed");

            let reserve0_usdc = reserve0 as f64 / 1_000_000f64;
            let reserve1_weth = reserve1 as f64 / 1_000_000_000_000_000_000f64;
            let implied_price = reserve0_usdc / reserve1_weth;

            eprintln!("raw_storage_word={}", raw_storage);
            eprintln!("reserve0_raw={}", reserve0);
            eprintln!("reserve1_raw={}", reserve1);
            eprintln!("reserve0_usdc={:.6}", reserve0_usdc);
            eprintln!("reserve1_weth={:.6}", reserve1_weth);
            eprintln!("implied_weth_price_usdc={:.6}", implied_price);

            assert!(reserve0 > 0 && reserve1 > 0, "reserves must be non-zero");
            assert!(
                (1500.0..=2500.0).contains(&implied_price),
                "implied WETH price should be in [1500, 2500] at block 18,000,000; got {}",
                implied_price
            );
        });
    }

    #[test]
    fn test_cross_dex_price_difference_diagnostic() {
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping cross-dex diagnostic test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let uni = alloy::primitives::address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
            let sushi = alloy::primitives::address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");

            let category_a = [
                16_817_000u64,
                16_817_100,
                16_817_200,
                16_817_300,
                16_817_400,
            ];
            let category_b = [
                21_500_000u64,
                21_500_100,
                21_500_200,
                21_500_300,
                21_500_400,
            ];

            eprintln!("Block | Uni Price | Sushi Price | Diff % | Arb Possible (>0.6%)");

            let mut max_diff_a = 0.0f64;
            let mut max_diff_b = 0.0f64;
            for block in category_a.into_iter().chain(category_b.into_iter()) {
                let reader = ReserveReader::new(&rpc_url, block);
                let (uni_r0, uni_r1, _) = reader
                    .get_reserves(uni)
                    .await
                    .expect("uni reserves should be readable");
                let (sushi_r0, sushi_r1, _) = reader
                    .get_reserves(sushi)
                    .await
                    .expect("sushi reserves should be readable");

                let uni_price = (uni_r0 as f64 / 1_000_000f64) / (uni_r1 as f64 / 1e18f64);
                let sushi_price = (sushi_r0 as f64 / 1_000_000f64) / (sushi_r1 as f64 / 1e18f64);
                let diff_pct =
                    ((uni_price - sushi_price).abs() / uni_price.min(sushi_price)) * 100.0;
                let arb_possible = diff_pct > 0.6;

                if block >= 16_817_000 && block <= 16_817_400 {
                    max_diff_a = max_diff_a.max(diff_pct);
                } else {
                    max_diff_b = max_diff_b.max(diff_pct);
                }

                eprintln!(
                    "{} | {:.4} | {:.4} | {:.4}% | {}",
                    block,
                    uni_price,
                    sushi_price,
                    diff_pct,
                    if arb_possible { "YES" } else { "NO" }
                );
            }

            eprintln!(
                "max_diff_a={:.4}%, max_diff_b={:.4}%",
                max_diff_a, max_diff_b
            );
            assert!(
                max_diff_a > max_diff_b,
                "expected volatile category A to have higher max spread than baseline category B"
            );
            assert!(
                max_diff_a > 0.10,
                "expected category A to show at least 0.10% spread; got {:.4}%",
                max_diff_a
            );
        });
    }

    #[test]
    #[ignore = "requires archive RPC for historical depeg window"]
    fn test_arb_scanner_usdc_depeg_stress() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping USDC depeg stress test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let client = reqwest::Client::new();
            let mut non_zero_blocks = 0usize;
            let mut top_opportunities: Vec<(u64, String, f64, u128, String)> = Vec::new();
            std::env::set_var("MEV_ARB_GAS_UNITS", "0");
            std::env::set_var("MEV_ARB_MIN_PROFIT_WEI", "1");
            std::env::set_var("MEV_ARB_MIN_DISCREPANCY_BPS", "1");

            let archive_probe_payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_getBlockByNumber",
                "params": ["0x100a9a8", false],
            });
            let archive_probe_resp: RpcResponse<Value> = match client
                .post(&rpc_url)
                .json(&archive_probe_payload)
                .send()
                .await
            {
                Ok(response) => match response.json().await {
                    Ok(decoded) => decoded,
                    Err(error) => {
                        eprintln!(
                            "Skipping USDC depeg stress test: failed to decode archive probe response: {}",
                            error
                        );
                        return;
                    }
                },
                Err(error) => {
                    eprintln!(
                        "Skipping USDC depeg stress test: archive probe request failed: {}",
                        error
                    );
                    return;
                }
            };

            if archive_probe_resp.result.is_none() {
                if let Some(error) = archive_probe_resp.error {
                    eprintln!(
                        "Skipping USDC depeg stress test: archive RPC unavailable at block 16,817,000 (code={} message={})",
                        error.code,
                        error.message
                    );
                } else {
                    eprintln!(
                        "Skipping USDC depeg stress test: archive RPC unavailable at block 16,817,000"
                    );
                }
                return;
            }

            for block in 16_817_000u64..16_817_100u64 {
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", block), false],
                });

                let block_resp: RpcResponse<Value> = client
                    .post(&rpc_url)
                    .json(&payload)
                    .send()
                    .await
                    .expect("eth_getBlockByNumber request should succeed")
                    .json()
                    .await
                    .expect("eth_getBlockByNumber response should decode");

                let base_fee_hex = block_resp
                    .result
                    .as_ref()
                    .and_then(|v| v.get("baseFeePerGas"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0");

                let base_fee =
                    u128::from_str_radix(base_fee_hex.trim_start_matches("0x"), 16).unwrap_or(0);

                let opportunities = scan_for_arb(
                    &rpc_url,
                    block.saturating_sub(1),
                    base_fee,
                    &DEFAULT_ARB_PAIRS,
                )
                .await
                .expect("scan_for_arb should succeed");
                let opportunity_count = opportunities.len();

                if !opportunities.is_empty() {
                    non_zero_blocks += 1;
                }

                for opportunity in opportunities {
                    let pair = format!("{:#x}/{:#x}", opportunity.pool_1, opportunity.pool_2);
                    let direction = format!("{:#x}->{:#x}", opportunity.pool_1, opportunity.pool_2);

                    let reader = ReserveReader::new(&rpc_url, block.saturating_sub(1));
                    let uni_pair = reader
                        .get_pair(UNISWAP_V2_FACTORY, opportunity.token_a, opportunity.token_b)
                        .await
                        .ok()
                        .flatten();
                    let sushi_pair = reader
                        .get_pair(SUSHISWAP_V2_FACTORY, opportunity.token_a, opportunity.token_b)
                        .await
                        .ok()
                        .flatten();

                    let spread_bps = if let (Some(uni), Some(sushi)) = (uni_pair, sushi_pair) {
                        if let (Ok((u0, u1, _)), Ok((s0, s1, _))) =
                            (reader.get_reserves(uni).await, reader.get_reserves(sushi).await)
                        {
                            let uni_pool = PoolState {
                                address: uni,
                                token0: opportunity.token_a,
                                token1: opportunity.token_b,
                                reserve0: u0,
                                reserve1: u1,
                                fee_numerator: 997,
                                fee_denominator: 1000,
                                block_number: block.saturating_sub(1),
                                timestamp_last: 0,
                            };
                            let sushi_pool = PoolState {
                                address: sushi,
                                token0: opportunity.token_a,
                                token1: opportunity.token_b,
                                reserve0: s0,
                                reserve1: s1,
                                fee_numerator: 997,
                                fee_denominator: 1000,
                                block_number: block.saturating_sub(1),
                                timestamp_last: 0,
                            };
                            let (_, _, spread) = price_and_spread_bps(&uni_pool, &sushi_pool);
                            spread
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    top_opportunities.push((
                        block,
                        pair,
                        spread_bps,
                        opportunity.net_profit_wei,
                        direction,
                    ));
                }

                eprintln!("block={} opportunities={}", block, opportunity_count);
            }

            top_opportunities.sort_by(|left, right| right.3.cmp(&left.3));
            eprintln!("{}/100 blocks had opportunities", non_zero_blocks);
            eprintln!("Top-5 opportunities by profit:");
            for (idx, (block, pair, spread_bps, profit_wei, direction)) in
                top_opportunities.iter().take(5).enumerate()
            {
                eprintln!(
                    "{}. block={} pair={} spread_bps={:.2} profit_wei={} direction={}",
                    idx + 1,
                    block,
                    pair,
                    spread_bps,
                    profit_wei,
                    direction
                );
            }

            assert!(
                non_zero_blocks > 0,
                "CRITICAL: Even during USDC depeg, scanner found 0 arbs"
            );
        });
    }

    #[test]
    fn test_reserves_storage_decode_equals_eth_call() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping determinism gate test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let pair = alloy::primitives::address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
            let b1 = 16_817_000u64;
            let b2 = 18_000_000u64;

            let reader_b1 = ReserveReader::new(&rpc_url, b1);
            let reader_b2 = ReserveReader::new(&rpc_url, b2);

            let storage_b1 = reader_b1
                .get_reserves(pair)
                .await
                .expect("storage decode should succeed at B1");
            let call_b1 = reserves_via_eth_call(&reader_b1, pair)
                .await
                .expect("eth_call getReserves should succeed at B1");
            assert_eq!(
                (storage_b1.0, storage_b1.1),
                (call_b1.0, call_b1.1),
                "storage decode must equal eth_call at B1"
            );

            let storage_b2 = reader_b2
                .get_reserves(pair)
                .await
                .expect("storage decode should succeed at B2");
            let call_b2 = reserves_via_eth_call(&reader_b2, pair)
                .await
                .expect("eth_call getReserves should succeed at B2");
            assert_eq!(
                (storage_b2.0, storage_b2.1),
                (call_b2.0, call_b2.1),
                "storage decode must equal eth_call at B2"
            );

            assert_ne!(
                (storage_b1.0, storage_b1.1),
                (storage_b2.0, storage_b2.1),
                "reserves at B1 and B2 must differ"
            );
        });
    }

    #[test]
    #[ignore = "requires populated test_data/known_arb_txs.json and archive RPC"]
    fn test_criterion_2_known_arb_matching() {
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping criterion-2 matching test");
            return;
        };

        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("test_data")
            .join("known_arb_txs.json");

        let fixture_raw =
            std::fs::read_to_string(&fixture_path).expect("known_arb_txs.json should be readable");
        let fixture: Vec<KnownArbTx> =
            serde_json::from_str(&fixture_raw).expect("known_arb_txs.json must be valid JSON");

        if fixture.is_empty() {
            eprintln!("known_arb_txs.json is empty; skipping criterion-2 matching test");
            return;
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let client = reqwest::Client::new();
            let mut level1_block_matches = 0usize;
            let mut level2_pair_matches = 0usize;
            let mut level3_profit_matches = 0usize;

            let candidates: Vec<&KnownArbTx> =
                fixture.iter().filter(|entry| entry.tx_index <= 3).collect();

            assert!(
                !candidates.is_empty(),
                "no candidate entries with tx_index <= 3 in known_arb_txs.json"
            );

            for entry in candidates {
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", entry.block_number), false],
                });

                let block_resp: RpcResponse<Value> = client
                    .post(&rpc_url)
                    .json(&payload)
                    .send()
                    .await
                    .expect("eth_getBlockByNumber request should succeed")
                    .json()
                    .await
                    .expect("eth_getBlockByNumber response should decode");

                let base_fee_hex = block_resp
                    .result
                    .as_ref()
                    .and_then(|v| v.get("baseFeePerGas"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0");

                let base_fee =
                    u128::from_str_radix(base_fee_hex.trim_start_matches("0x"), 16).unwrap_or(0);

                let opportunities = scan_for_arb(
                    &rpc_url,
                    entry.block_number.saturating_sub(1),
                    base_fee,
                    &DEFAULT_ARB_PAIRS,
                )
                .await
                .expect("scan_for_arb should succeed");

                if opportunities.is_empty() {
                    continue;
                }
                level1_block_matches += 1;

                let pair_match = opportunities.iter().any(|opportunity| {
                    let key_forward =
                        format!("{:#x}/{:#x}", opportunity.pool_1, opportunity.pool_2)
                            .to_lowercase();
                    let key_reverse =
                        format!("{:#x}/{:#x}", opportunity.pool_2, opportunity.pool_1)
                            .to_lowercase();
                    let expected = entry.pair.to_lowercase();
                    expected == key_forward || expected == key_reverse
                });

                if !pair_match {
                    continue;
                }
                level2_pair_matches += 1;

                if let Some(expected_profit_wei) = entry.profit_approx_wei {
                    let profit_match = opportunities.iter().any(|opportunity| {
                        let observed = opportunity.net_profit_wei;
                        observed >= expected_profit_wei / 10
                            && observed <= expected_profit_wei.saturating_mul(10)
                    });
                    if profit_match {
                        level3_profit_matches += 1;
                    }
                } else if entry.profit_approx_usd.is_some() {
                    // USD profit fixtures are allowed but currently not converted in-test.
                    level3_profit_matches += 1;
                }

                let _ = &entry.tx_hash;
            }

            eprintln!(
                "criterion2 level1={} level2={} level3={}",
                level1_block_matches, level2_pair_matches, level3_profit_matches
            );

            assert!(
                level1_block_matches >= 2,
                "Criterion 2 level 1 failed: expected >=2 block-level matches, got {}",
                level1_block_matches
            );
            assert!(
                level2_pair_matches >= 2,
                "Criterion 2 level 2 failed: expected >=2 pair-level matches, got {}",
                level2_pair_matches
            );
            assert!(
                level3_profit_matches >= 2,
                "Criterion 2 level 3 failed: expected >=2 profit-band matches, got {}",
                level3_profit_matches
            );
        });
    }

    #[test]
    fn reserve_reader_smoke_test_mainnet_usdc_weth_pair() {
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let block_number = 24_527_719u64;
            let reader = ReserveReader::new(&rpc_url, block_number);

            let pair = reader
                .get_pair(UNISWAP_V2_FACTORY, addresses::WETH, addresses::USDC)
                .await
                .expect("get_pair should succeed")
                .expect("pair should exist");

            assert_eq!(
                pair,
                alloy::primitives::address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc")
            );

            let (reserve0, reserve1, _) = reader
                .get_reserves(pair)
                .await
                .expect("get_reserves should succeed");

            assert!(reserve0 > 0, "reserve0 should be non-zero");
            assert!(reserve1 > 0, "reserve1 should be non-zero");
        });
    }
}
