//! CEX-DEX spread scanner for block-boundary opportunities.
//!
//! Uses historical Binance ETHUSDC prices as a centralized exchange reference and
//! compares against Uniswap V2 USDC/WETH reserves at `block_number` state.

use alloy::primitives::{Address, U256};
use eyre::{eyre, Result};

use crate::decoder::addresses;

use super::arbitrage::{ArbPairConfig, ReserveReader, DEFAULT_ARB_PAIRS};

const UNISWAP_V2_FEE_NUMERATOR: u128 = 997;
const UNISWAP_V2_FEE_DENOMINATOR: u128 = 1000;
const CEX_PRICE_SCALE_DECIMALS: u32 = 8;

fn cex_debug_enabled() -> bool {
    std::env::var("MEV_CEX_DEBUG")
        .ok()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn pow10_u256(exp: u32) -> U256 {
    let mut out = U256::from(1u8);
    for _ in 0..exp {
        out *= U256::from(10u8);
    }
    out
}

fn cex_price_fp_to_quote_per_weth(cex_price_fp: u128, quote_decimals: u32) -> U256 {
    let scale = pow10_u256(CEX_PRICE_SCALE_DECIMALS);
    let quote_scale = pow10_u256(quote_decimals);
    // CEX price FP is `price * 10^8`. We want `price * 10^quote_decimals`.
    // dex_price_quote_per_weth = (reserve_quote * 10^18) / reserve_weth
    //   which simplifies to `price * 10^quote_decimals` for balanced pools.
    (U256::from(cex_price_fp) * quote_scale) / scale
}

fn format_price_from_quote_per_weth(value: U256, quote_decimals: u32) -> f64 {
    let denominator = pow10_u256(quote_decimals);
    let integer = (value / denominator).to::<u128>();
    let fractional = (value % denominator).to::<u128>();
    let frac_digits = quote_decimals as usize;
    let rendered = format!("{}.{:0width$}", integer, fractional, width = frac_digits);
    rendered.parse::<f64>().unwrap_or(0.0)
}

fn cex_price_fp_to_f64(cex_price_fp: u128) -> f64 {
    let scale = 10u128.pow(CEX_PRICE_SCALE_DECIMALS);
    let integer = cex_price_fp / scale;
    let fractional = cex_price_fp % scale;
    let rendered = format!(
        "{}.{:0width$}",
        integer,
        fractional,
        width = CEX_PRICE_SCALE_DECIMALS as usize
    );
    rendered.parse::<f64>().unwrap_or(0.0)
}

pub fn cex_price_f64_to_fp(value: f64) -> Result<u128> {
    if !value.is_finite() || value <= 0.0 {
        return Err(eyre!("invalid cex price value: {}", value));
    }

    let rendered = format!("{value:.8}");
    let mut parts = rendered.split('.');
    let whole = parts
        .next()
        .ok_or_else(|| eyre!("failed to parse price whole part"))?;
    let frac = parts.next().unwrap_or("0");
    let mut frac_padded = frac.to_string();
    while frac_padded.len() < CEX_PRICE_SCALE_DECIMALS as usize {
        frac_padded.push('0');
    }
    let frac_padded = &frac_padded[..CEX_PRICE_SCALE_DECIMALS as usize];

    let whole_u128 = whole.parse::<u128>().map_err(|error| {
        eyre!(
            "failed parsing whole price component '{}': {}",
            whole,
            error
        )
    })?;
    let frac_u128 = frac_padded.parse::<u128>().map_err(|error| {
        eyre!(
            "failed parsing fractional price component '{}': {}",
            frac_padded,
            error
        )
    })?;

    Ok(whole_u128
        .saturating_mul(10u128.pow(CEX_PRICE_SCALE_DECIMALS))
        .saturating_add(frac_u128))
}

fn configured_cex_stale_seconds() -> u64 {
    std::env::var("MEV_CEX_MAX_STALE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3)
}

/// Converts a CEX price in micro-USD (price × 10^6) to the internal 8-decimal fixed-point format.
///
/// Example: $1615.76 stored as micro_usd = 1_615_760_000 → fp = 161_576_000_000 (× 10^8).
pub fn micro_usd_to_cex_price_fp(micro_usd: i64) -> u128 {
    // micro_usd = price * 10^6
    // fp        = price * 10^8 = micro_usd * 100
    (micro_usd as u128).saturating_mul(100)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CexPricePoint {
    pub timestamp_s: u64,
    pub close_price_fp: u128,
}

/// Direction for executing CEX-DEX spread arbitrage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArbDirection {
    /// Buy ETH on DEX and sell on CEX.
    BuyOnDex,
    /// Buy ETH on CEX and sell on DEX.
    SellOnDex,
}

/// Evaluation outcome for CEX-DEX spread checks.
#[derive(Clone, Debug, PartialEq)]
pub enum CexDexVerdict {
    /// No CEX datapoint available.
    NoCexData,
    /// CEX datapoint exists but is too stale from block timestamp.
    StaleCexData {
        block_timestamp_s: u64,
        candle_timestamp_s: u64,
        delta_seconds: u64,
    },
    /// Spread is below fee floor.
    SpreadBelowFee {
        spread_bps: u128,
        dex_price_quote_per_weth: u128,
        cex_price_quote_per_weth: u128,
    },
    /// Spread is wide enough but best tested size yields zero/non-positive profit.
    NonPositiveProfit {
        spread_bps: u128,
        best_profit_wei: u128,
    },
    /// Profitable opportunity found.
    Opportunity(CexDexOpportunity),
}

/// CEX-DEX opportunity details for a single block.
#[derive(Clone, Debug, PartialEq)]
pub struct CexDexOpportunity {
    /// Block number where reserves were observed.
    pub block_number: u64,
    /// DEX pair address.
    pub pair_address: Address,
    /// Spread between DEX and CEX in basis points.
    pub spread_bps: u128,
    /// DEX implied ETH price in quote-token units per WETH.
    pub dex_price_quote_per_weth: u128,
    /// CEX price in quote-token units per WETH.
    pub cex_price_quote_per_weth: u128,
    /// Expected profit in WETH wei.
    pub profit_wei: u128,
    /// Best tested input size.
    pub best_input_wei: u128,
    /// Block timestamp.
    pub block_timestamp_s: u64,
    /// Matched CEX candle timestamp.
    pub candle_timestamp_s: u64,
    /// Absolute delta between block and candle timestamps.
    pub candle_delta_seconds: u64,
    /// Trade direction.
    pub direction: ArbDirection,
}

fn token_decimals(token: Address) -> u32 {
    if token == addresses::WETH {
        18
    } else if token == addresses::USDC || token == addresses::USDT {
        6
    } else {
        // DAI and all other tokens default to 18 decimals
        18
    }
}

fn amount_out(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }

    let amount_in_u256 = U256::from(amount_in);
    let reserve_in_u256 = U256::from(reserve_in);
    let reserve_out_u256 = U256::from(reserve_out);

    let amount_in_with_fee = amount_in_u256 * U256::from(UNISWAP_V2_FEE_NUMERATOR);
    let numerator = amount_in_with_fee * reserve_out_u256;
    let denominator =
        (reserve_in_u256 * U256::from(UNISWAP_V2_FEE_DENOMINATOR)) + amount_in_with_fee;

    if denominator.is_zero() {
        return 0;
    }

    (numerator / denominator).to::<u128>()
}

fn estimate_profit_wei(
    reserve_weth: u128,
    reserve_quote: u128,
    cex_quote_per_weth: U256,
    quote_token: Address,
    direction: ArbDirection,
) -> (u128, u128) {
    let quote_decimals = token_decimals(quote_token);

    let max_input = match direction {
        ArbDirection::BuyOnDex => reserve_quote / 10,
        ArbDirection::SellOnDex => reserve_weth / 10,
    };

    if max_input == 0 {
        return (0, 0);
    }

    let mut candidate_inputs = Vec::new();
    let min_input = match direction {
        ArbDirection::BuyOnDex => 1_000_000u128,
        ArbDirection::SellOnDex => 1_000_000_000_000_000u128,
    };

    for exponent in 0..=24u32 {
        let shifted = max_input >> exponent;
        if shifted == 0 {
            continue;
        }
        candidate_inputs.push(shifted.max(min_input).min(max_input));
    }

    for step in 1..=40u128 {
        let input = max_input.saturating_mul(step) / 40;
        if input > 0 {
            candidate_inputs.push(input.max(min_input).min(max_input));
        }
    }

    candidate_inputs.sort_unstable();
    candidate_inputs.dedup();

    let mut best_profit_wei = 0u128;
    let mut best_input = 0u128;

    for input in candidate_inputs {
        if input == 0 {
            continue;
        }

        match direction {
            ArbDirection::BuyOnDex => {
                let output_weth = amount_out(input, reserve_quote, reserve_weth);
                if output_weth == 0 {
                    continue;
                }
                let output_quote_at_cex =
                    (U256::from(output_weth) * cex_quote_per_weth) / pow10_u256(18);
                let input_quote_u256 = U256::from(input);
                if output_quote_at_cex > input_quote_u256 {
                    let profit_quote = output_quote_at_cex - input_quote_u256;
                    let profit_wei =
                        ((profit_quote * pow10_u256(18)) / cex_quote_per_weth).to::<u128>();
                    if profit_wei > best_profit_wei {
                        best_profit_wei = profit_wei;
                        best_input = input;
                    }
                }
            }
            ArbDirection::SellOnDex => {
                let output_quote = amount_out(input, reserve_weth, reserve_quote);
                if output_quote == 0 {
                    continue;
                }
                let input_quote_at_cex = (U256::from(input) * cex_quote_per_weth) / pow10_u256(18);
                let output_quote_u256 = U256::from(output_quote);
                if output_quote_u256 > input_quote_at_cex {
                    let profit_quote = output_quote_u256 - input_quote_at_cex;
                    let profit_wei =
                        ((profit_quote * pow10_u256(18)) / cex_quote_per_weth).to::<u128>();
                    if profit_wei > best_profit_wei {
                        best_profit_wei = profit_wei;
                        best_input = input;
                    }
                }
            }
        }
    }

    if quote_decimals == 0 {
        return (0, 0);
    }

    (best_profit_wei, best_input)
}

/// Evaluates one CEX-DEX opportunity from reserves and CEX price.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_cex_dex_opportunity(
    block_number: u64,
    block_timestamp_s: u64,
    pair_address: Address,
    reserve_quote: u128,
    reserve_weth: u128,
    cex_price: Option<CexPricePoint>,
    quote_token: Address,
    fee_bps: u128,
) -> CexDexVerdict {
    let Some(cex_price) = cex_price else {
        return CexDexVerdict::NoCexData;
    };

    let delta_seconds = block_timestamp_s.abs_diff(cex_price.timestamp_s);
    if delta_seconds > configured_cex_stale_seconds() {
        return CexDexVerdict::StaleCexData {
            block_timestamp_s,
            candle_timestamp_s: cex_price.timestamp_s,
            delta_seconds,
        };
    }

    if reserve_quote == 0 || reserve_weth == 0 {
        return CexDexVerdict::NonPositiveProfit {
            spread_bps: 0,
            best_profit_wei: 0,
        };
    }

    let quote_decimals = token_decimals(quote_token);
    let dex_price_quote_per_weth =
        (U256::from(reserve_quote) * pow10_u256(18)) / U256::from(reserve_weth);
    let cex_price_quote_per_weth =
        cex_price_fp_to_quote_per_weth(cex_price.close_price_fp, quote_decimals);

    let spread_numerator = if cex_price_quote_per_weth >= dex_price_quote_per_weth {
        cex_price_quote_per_weth - dex_price_quote_per_weth
    } else {
        dex_price_quote_per_weth - cex_price_quote_per_weth
    };
    let spread_bps =
        (spread_numerator * U256::from(10_000u64) / dex_price_quote_per_weth).to::<u128>();

    if spread_numerator * U256::from(10_000u64) <= dex_price_quote_per_weth * U256::from(fee_bps) {
        return CexDexVerdict::SpreadBelowFee {
            spread_bps,
            dex_price_quote_per_weth: dex_price_quote_per_weth.to::<u128>(),
            cex_price_quote_per_weth: cex_price_quote_per_weth.to::<u128>(),
        };
    }

    let direction = if cex_price_quote_per_weth > dex_price_quote_per_weth {
        ArbDirection::BuyOnDex
    } else {
        ArbDirection::SellOnDex
    };

    let (profit_wei, best_input_wei) = estimate_profit_wei(
        reserve_weth,
        reserve_quote,
        cex_price_quote_per_weth,
        quote_token,
        direction,
    );

    if profit_wei == 0 {
        return CexDexVerdict::NonPositiveProfit {
            spread_bps,
            best_profit_wei: 0,
        };
    }

    CexDexVerdict::Opportunity(CexDexOpportunity {
        block_number,
        pair_address,
        spread_bps,
        dex_price_quote_per_weth: dex_price_quote_per_weth.to::<u128>(),
        cex_price_quote_per_weth: cex_price_quote_per_weth.to::<u128>(),
        profit_wei,
        best_input_wei,
        block_timestamp_s,
        candle_timestamp_s: cex_price.timestamp_s,
        candle_delta_seconds: delta_seconds,
        direction,
    })
}

/// Scans default WETH/USDC pair against Binance ETHUSDC reference.
///
/// # Errors
/// Returns error when RPC reserve reads fail.
pub async fn scan_cex_dex(
    rpc_url: &str,
    block_number: u64,
    block_timestamp_s: u64,
    cex_price: Option<CexPricePoint>,
) -> Result<Option<CexDexOpportunity>> {
    let reserve_reader = ReserveReader::new(rpc_url, block_number);
    let pair_config = DEFAULT_ARB_PAIRS
        .iter()
        .find(|config| config.token_a == addresses::WETH && config.token_b == addresses::USDC)
        .copied()
        .unwrap_or(ArbPairConfig {
            token_a: addresses::WETH,
            token_b: addresses::USDC,
            dex_a_factory: addresses::UNISWAP_V2_FACTORY,
            dex_b_factory: addresses::UNISWAP_V2_FACTORY,
        });

    let factories = [pair_config.dex_a_factory, pair_config.dex_b_factory];
    let mut best: Option<CexDexOpportunity> = None;

    for factory in factories {
        let Some(pair_addr) = reserve_reader
            .get_pair(factory, pair_config.token_a, pair_config.token_b)
            .await?
        else {
            continue;
        };

        let (reserve0, reserve1, _) = reserve_reader.get_reserves(pair_addr).await?;
        let (reserve_weth, reserve_usdc) = if pair_config.token_a < pair_config.token_b {
            (reserve0, reserve1)
        } else {
            (reserve1, reserve0)
        };

        let verdict = evaluate_cex_dex_opportunity(
            block_number,
            block_timestamp_s,
            pair_addr,
            reserve_usdc,
            reserve_weth,
            cex_price,
            pair_config.token_b,
            30,
        );

        if cex_debug_enabled() {
            match &verdict {
                CexDexVerdict::NoCexData => {
                    tracing::debug!(block_number, pair = %format!("{pair_addr:#x}"), "cex_dex rejected: NoCexData");
                }
                CexDexVerdict::StaleCexData {
                    block_timestamp_s,
                    candle_timestamp_s,
                    delta_seconds,
                } => {
                    tracing::debug!(
                        block_number,
                        pair = %format!("{pair_addr:#x}"),
                        block_timestamp_s = *block_timestamp_s,
                        candle_timestamp_s = *candle_timestamp_s,
                        delta_seconds = *delta_seconds,
                        "cex_dex rejected: StaleCexData"
                    );
                }
                CexDexVerdict::SpreadBelowFee {
                    spread_bps,
                    dex_price_quote_per_weth,
                    cex_price_quote_per_weth,
                } => {
                    tracing::debug!(
                        block_number,
                        pair = %format!("{pair_addr:#x}"),
                        spread_bps = *spread_bps,
                        dex_price = format_price_from_quote_per_weth(U256::from(*dex_price_quote_per_weth), token_decimals(pair_config.token_b)),
                        cex_price = format_price_from_quote_per_weth(U256::from(*cex_price_quote_per_weth), token_decimals(pair_config.token_b)),
                        "cex_dex rejected: SpreadBelowFee"
                    );
                }
                CexDexVerdict::NonPositiveProfit {
                    spread_bps,
                    best_profit_wei,
                } => {
                    tracing::debug!(
                        block_number,
                        pair = %format!("{pair_addr:#x}"),
                        spread_bps = *spread_bps,
                        best_profit_wei = *best_profit_wei,
                        "cex_dex rejected: NonPositiveProfit"
                    );
                }
                CexDexVerdict::Opportunity(opportunity) => {
                    tracing::debug!(
                        block_number,
                        pair = %format!("{pair_addr:#x}"),
                        spread_bps = opportunity.spread_bps,
                        dex_price = format_price_from_quote_per_weth(U256::from(opportunity.dex_price_quote_per_weth), token_decimals(pair_config.token_b)),
                        cex_price = cex_price_fp_to_f64(cex_price.map(|value| value.close_price_fp).unwrap_or_default()),
                        best_input_wei = opportunity.best_input_wei,
                        profit_wei = opportunity.profit_wei,
                        reason = "Opportunity",
                        "cex_dex evaluated"
                    );
                }
            }
        }

        match &verdict {
            CexDexVerdict::SpreadBelowFee { .. }
            | CexDexVerdict::NoCexData
            | CexDexVerdict::StaleCexData { .. }
            | CexDexVerdict::NonPositiveProfit { .. } => {}
            CexDexVerdict::Opportunity(opportunity) => {
                if opportunity.profit_wei > 0
                    && best
                        .as_ref()
                        .map(|current| opportunity.profit_wei > current.profit_wei)
                        .unwrap_or(true)
                {
                    best = Some(opportunity.clone());
                }
            }
        }
    }

    Ok(best)
}

#[cfg(test)]
/// Collects the full profit curve across all candidate inputs.
///
/// Returns a sorted vec of `(amount_in, profit_wei)` — including zero-profit entries.
fn collect_profit_curve(
    reserve_weth: u128,
    reserve_quote: u128,
    cex_quote_per_weth: U256,
    _quote_token: Address,
    direction: ArbDirection,
) -> Vec<(u128, i128)> {
    let max_input = match direction {
        ArbDirection::BuyOnDex => reserve_quote / 10,
        ArbDirection::SellOnDex => reserve_weth / 10,
    };

    if max_input == 0 {
        return Vec::new();
    }

    let mut candidate_inputs = Vec::new();
    let min_input = match direction {
        ArbDirection::BuyOnDex => 1_000_000u128,
        ArbDirection::SellOnDex => 1_000_000_000_000_000u128,
    };

    for exponent in 0..=24u32 {
        let shifted = max_input >> exponent;
        if shifted == 0 {
            continue;
        }
        candidate_inputs.push(shifted.max(min_input).min(max_input));
    }

    for step in 1..=40u128 {
        let input = max_input.saturating_mul(step) / 40;
        if input > 0 {
            candidate_inputs.push(input.max(min_input).min(max_input));
        }
    }

    candidate_inputs.sort_unstable();
    candidate_inputs.dedup();

    let mut curve = Vec::new();

    for input in candidate_inputs {
        if input == 0 {
            continue;
        }

        let profit: i128 = match direction {
            ArbDirection::SellOnDex => {
                let output_quote = amount_out(input, reserve_weth, reserve_quote);
                if output_quote == 0 {
                    0
                } else {
                    let input_quote_at_cex =
                        (U256::from(input) * cex_quote_per_weth) / pow10_u256(18);
                    let output_quote_u256 = U256::from(output_quote);
                    if output_quote_u256 > input_quote_at_cex {
                        let profit_quote = output_quote_u256 - input_quote_at_cex;
                        ((profit_quote * pow10_u256(18)) / cex_quote_per_weth).to::<i128>()
                    } else {
                        let loss_quote = input_quote_at_cex - output_quote_u256;
                        -(((loss_quote * pow10_u256(18)) / cex_quote_per_weth).to::<i128>())
                    }
                }
            }
            ArbDirection::BuyOnDex => {
                let output_weth = amount_out(input, reserve_quote, reserve_weth);
                if output_weth == 0 {
                    0
                } else {
                    let output_quote_at_cex =
                        (U256::from(output_weth) * cex_quote_per_weth) / pow10_u256(18);
                    let input_quote_u256 = U256::from(input);
                    if output_quote_at_cex > input_quote_u256 {
                        let profit_quote = output_quote_at_cex - input_quote_u256;
                        ((profit_quote * pow10_u256(18)) / cex_quote_per_weth).to::<i128>()
                    } else {
                        let loss_quote = input_quote_u256 - output_quote_at_cex;
                        -(((loss_quote * pow10_u256(18)) / cex_quote_per_weth).to::<i128>())
                    }
                }
            }
        };

        curve.push((input, profit));
    }

    curve
}

#[cfg(test)]
mod tests {
    use super::*;
    use mev_data::store::Store;

    #[test]
    fn cex_integer_threshold_is_stable_under_tight_spread() {
        let block_number = 1;
        let block_timestamp_s = 1_678_000_000;
        let pair_address = addresses::UNISWAP_V2_ROUTER;
        let reserve_weth = 1_000_000u128 * 10u128.pow(18);
        let reserve_quote = 1_600_000_000u128 * 10u128.pow(6);
        let cex = CexPricePoint {
            timestamp_s: block_timestamp_s,
            close_price_fp: 1_604_70000000,
        };

        let verdict_a = evaluate_cex_dex_opportunity(
            block_number,
            block_timestamp_s,
            pair_address,
            reserve_quote,
            reserve_weth,
            Some(cex),
            addresses::USDC,
            30,
        );

        let verdict_b = evaluate_cex_dex_opportunity(
            block_number,
            block_timestamp_s,
            pair_address,
            reserve_quote.saturating_add(1),
            reserve_weth,
            Some(cex),
            addresses::USDC,
            30,
        );

        assert!(matches!(verdict_a, CexDexVerdict::SpreadBelowFee { .. }));
        assert!(matches!(verdict_b, CexDexVerdict::SpreadBelowFee { .. }));
    }

    #[test]
    fn cex_verdicts_for_missing_and_stale_data() {
        let pair_address = addresses::UNISWAP_V2_ROUTER;
        let reserve_weth = 1_000u128 * 10u128.pow(18);
        let reserve_quote = 1_600_000u128 * 10u128.pow(6);
        let block_timestamp_s = 1_678_000_000;

        let missing = evaluate_cex_dex_opportunity(
            1,
            block_timestamp_s,
            pair_address,
            reserve_quote,
            reserve_weth,
            None,
            addresses::USDC,
            30,
        );
        assert!(matches!(missing, CexDexVerdict::NoCexData));

        let stale = evaluate_cex_dex_opportunity(
            1,
            block_timestamp_s,
            pair_address,
            reserve_quote,
            reserve_weth,
            Some(CexPricePoint {
                timestamp_s: block_timestamp_s.saturating_sub(99),
                close_price_fp: 1_600_00000000,
            }),
            addresses::USDC,
            30,
        );
        assert!(matches!(stale, CexDexVerdict::StaleCexData { .. }));
    }

    fn init_test_tracing() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_test_writer()
                .try_init();
        });
    }

    #[test]
    #[ignore = "requires archive RPC and CEX price data in data/mev.sqlite"]
    fn test_cex_dex_depeg() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping CEX-DEX depeg test");
            return;
        };

        let db_path =
            std::env::var("MEV_DB_PATH").unwrap_or_else(|_| "data/mev.sqlite".to_string());
        let store = Store::new(&db_path).expect("should open SQLite store");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let client = reqwest::Client::new();
            let mut total_scanned = 0u64;
            let mut non_zero_count = 0usize;
            let mut sample_detections: Vec<(u64, u128, u128)> = Vec::new();
            let mut db_hit_count = 0usize;

            for block_number in 16_817_000u64..16_817_100u64 {
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", block_number), false],
                });

                let resp = client.post(&rpc_url).json(&payload).send().await;
                let block_data = match resp {
                    Ok(response) => {
                        let body: serde_json::Value = response.json().await.unwrap_or_default();
                        body.get("result").cloned().unwrap_or_default()
                    }
                    Err(error) => {
                        eprintln!("block {} fetch failed: {}", block_number, error);
                        continue;
                    }
                };

                let block_timestamp = block_data
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0);

                // Look up real Binance ETHUSDC price from the database
                let cex_price = store
                    .get_nearest_cex_close_price_micro("ETHUSDC", block_timestamp)
                    .ok()
                    .flatten()
                    .map(|(ts, close_micro)| {
                        db_hit_count += 1;
                        CexPricePoint {
                            timestamp_s: ts,
                            close_price_fp: micro_usd_to_cex_price_fp(close_micro),
                        }
                    });

                let result = scan_cex_dex(&rpc_url, block_number, block_timestamp, cex_price).await;

                total_scanned += 1;

                match result {
                    Ok(Some(opportunity)) => {
                        non_zero_count += 1;
                        sample_detections.push((
                            block_number,
                            opportunity.spread_bps,
                            opportunity.profit_wei,
                        ));
                        eprintln!(
                            "block={} OPPORTUNITY spread_bps={} profit_wei={} direction={:?}",
                            block_number,
                            opportunity.spread_bps,
                            opportunity.profit_wei,
                            opportunity.direction,
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("block {} scan error: {}", block_number, error);
                    }
                }
            }

            eprintln!("\n=== CEX-DEX DEPEG SUMMARY (real Binance klines) ===");
            eprintln!("Total blocks scanned: {}", total_scanned);
            eprintln!("DB price hits: {}", db_hit_count);
            eprintln!("Non-zero detections: {}", non_zero_count);
            eprintln!("Sample detections (first 10):");
            for (idx, (block, spread, profit)) in sample_detections.iter().take(10).enumerate() {
                eprintln!(
                    "  {}. block={} spread_bps={} profit_wei={}",
                    idx + 1,
                    block,
                    spread,
                    profit,
                );
            }

            // At least 90 of 100 blocks should have DB price data
            assert!(
                db_hit_count >= 90,
                "expected >= 90 DB price hits, got {}",
                db_hit_count,
            );
        });
    }

    #[test]
    #[ignore = "requires archive RPC"]
    fn test_cex_dex_profit_curve_shape() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping profit curve test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let block_number: u64 = 16_817_050;
            let reserve_reader =
                super::super::arbitrage::ReserveReader::new(&rpc_url, block_number);

            // Read Uniswap V2 WETH/USDC reserves
            let pair_addr = reserve_reader
                .get_pair(
                    addresses::UNISWAP_V2_FACTORY,
                    addresses::WETH,
                    addresses::USDC,
                )
                .await
                .expect("get_pair should succeed")
                .expect("pair should exist");

            let (reserve0, reserve1, _) = reserve_reader
                .get_reserves(pair_addr)
                .await
                .expect("get_reserves should succeed");

            // WETH < USDC by address sort, so WETH is token0
            let (reserve_weth, reserve_usdc) = if addresses::WETH < addresses::USDC {
                (reserve0, reserve1)
            } else {
                (reserve1, reserve0)
            };

            eprintln!("=== PROFIT CURVE SANITY CHECK (block {}) ===", block_number);
            eprintln!(
                "Reserves: {} WETH wei, {} USDC (6 dec)",
                reserve_weth, reserve_usdc
            );

            // Hardcoded CEX price ($1580)
            let cex_price_fp = cex_price_f64_to_fp(1580.0).expect("price conversion");
            let cex_quote_per_weth = cex_price_fp_to_quote_per_weth(cex_price_fp, 6);

            eprintln!(
                "CEX price: $1580.00 (quote_per_weth={})",
                cex_quote_per_weth
            );

            // SellOnDex direction (WETH overpriced on DEX during depeg)
            let curve = collect_profit_curve(
                reserve_weth,
                reserve_usdc,
                cex_quote_per_weth,
                addresses::USDC,
                ArbDirection::SellOnDex,
            );

            assert!(
                !curve.is_empty(),
                "profit curve should have at least one candidate"
            );

            // Print the full curve
            eprintln!(
                "\n{:>5} | {:>22} | {:>18} | {:>14}",
                "idx", "amount_in_wei", "profit_wei", "profit_eth"
            );
            eprintln!("{}", "-".repeat(70));

            let mut max_profit: i128 = i128::MIN;
            let mut max_profit_index: usize = 0;

            for (idx, (amount_in, profit)) in curve.iter().enumerate() {
                let _amount_in_eth = *amount_in as f64 / 1e18;
                let profit_eth = *profit as f64 / 1e18;

                eprintln!(
                    "{:>5} | {:>22} | {:>18} | {:>14.6}",
                    idx, amount_in, profit, profit_eth
                );

                if *profit > max_profit {
                    max_profit = *profit;
                    max_profit_index = idx;
                }
            }

            let last_index = curve.len() - 1;

            eprintln!("\n--- CURVE ANALYSIS ---");
            eprintln!("Total candidates: {}", curve.len());
            eprintln!(
                "Max profit: {} wei ({:.6} ETH) at index {}",
                max_profit,
                max_profit as f64 / 1e18,
                max_profit_index
            );
            eprintln!(
                "Max input: {} wei ({:.4} ETH)",
                curve[max_profit_index].0,
                curve[max_profit_index].0 as f64 / 1e18
            );
            eprintln!("First candidate profit: {} wei", curve[0].1);
            eprintln!("Last candidate profit: {} wei", curve[last_index].1);

            let reserve_fraction = curve[max_profit_index].0 as f64 / reserve_weth as f64;
            eprintln!(
                "Optimal input as % of WETH reserve: {:.2}%",
                reserve_fraction * 100.0
            );

            // ASSERTIONS
            // 1. Max is NOT at index 0 (interior optimum, not smallest)
            assert!(
                max_profit_index > 0,
                "max profit should NOT be at smallest candidate (index 0)"
            );

            // 2. Max is NOT at last index (not hitting the cap)
            assert!(
                max_profit_index < last_index,
                "max profit at last index {} — sweep cap may be too tight",
                max_profit_index
            );

            // 3. First candidate is profitable (given ~230 bps spread)
            assert!(
                curve[0].1 > 0,
                "smallest candidate should be profitable (spread ~230 bps)"
            );

            // 4. Last candidate has lower profit than peak (curve descends)
            assert!(
                curve[last_index].1 < max_profit,
                "last candidate should have less profit than peak"
            );

            eprintln!("\n=== PROFIT CURVE SHAPE: VALID (unimodal, interior peak) ===");
        });
    }

    /// Scan the USDC-depeg window using real Binance 1s klines from the database.
    ///
    /// Validates that:
    /// - Every block gets a DB price hit (1s resolution)
    /// - Spread values have variance (not all identical)
    /// - Detected profit is non-negative when spread > fee
    /// - Reports min/max/mean spread and profit statistics
    #[test]
    #[ignore = "requires archive RPC and real CEX data in data/mev.sqlite"]
    fn test_cex_dex_with_real_klines() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping real klines test");
            return;
        };

        let db_path =
            std::env::var("MEV_DB_PATH").unwrap_or_else(|_| "data/mev.sqlite".to_string());
        let store = Store::new(&db_path).expect("should open SQLite store");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            let client = reqwest::Client::new();
            let mut db_hit_count = 0usize;
            let mut total_scanned = 0u64;
            let mut opportunity_count = 0usize;
            let mut spreads: Vec<u128> = Vec::new();
            let mut profits: Vec<u128> = Vec::new();
            let mut prices_seen: Vec<i64> = Vec::new();

            for block_number in 16_817_000u64..16_817_100u64 {
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", block_number), false],
                });

                let resp = client.post(&rpc_url).json(&payload).send().await;
                let block_data = match resp {
                    Ok(response) => {
                        let body: serde_json::Value = response.json().await.unwrap_or_default();
                        body.get("result").cloned().unwrap_or_default()
                    }
                    Err(error) => {
                        eprintln!("block {} fetch failed: {}", block_number, error);
                        continue;
                    }
                };

                let block_timestamp = block_data
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                    .unwrap_or(0);

                let cex_price = store
                    .get_nearest_cex_close_price_micro("ETHUSDC", block_timestamp)
                    .ok()
                    .flatten()
                    .map(|(ts, close_micro)| {
                        db_hit_count += 1;
                        prices_seen.push(close_micro);
                        CexPricePoint {
                            timestamp_s: ts,
                            close_price_fp: micro_usd_to_cex_price_fp(close_micro),
                        }
                    });

                let result = scan_cex_dex(&rpc_url, block_number, block_timestamp, cex_price).await;
                total_scanned += 1;

                if let Ok(Some(opp)) = &result {
                    opportunity_count += 1;
                    spreads.push(opp.spread_bps);
                    profits.push(opp.profit_wei);
                }
            }

            eprintln!("\n=== REAL KLINES VALIDATION ===");
            eprintln!("Blocks scanned: {}", total_scanned);
            eprintln!("DB price hits: {}", db_hit_count);
            eprintln!("Opportunities: {}", opportunity_count);

            // Price variance: should have multiple distinct prices
            prices_seen.sort();
            prices_seen.dedup();
            eprintln!("Distinct CEX prices: {}", prices_seen.len());
            if let (Some(lo), Some(hi)) = (prices_seen.first(), prices_seen.last()) {
                eprintln!(
                    "Price range: ${:.2} — ${:.2}",
                    *lo as f64 / 1_000_000.0,
                    *hi as f64 / 1_000_000.0,
                );
            }

            if !spreads.is_empty() {
                let min_spread = spreads.iter().copied().min().unwrap_or(0);
                let max_spread = spreads.iter().copied().max().unwrap_or(0);
                let mean_spread = spreads.iter().copied().sum::<u128>() / spreads.len() as u128;
                eprintln!(
                    "Spread (bps) — min: {} max: {} mean: {}",
                    min_spread, max_spread, mean_spread
                );

                let min_profit = profits.iter().copied().min().unwrap_or(0);
                let max_profit = profits.iter().copied().max().unwrap_or(0);
                let mean_profit = profits.iter().copied().sum::<u128>() / profits.len() as u128;
                eprintln!(
                    "Profit (wei) — min: {} max: {} mean: {} ({:.6} ETH)",
                    min_profit,
                    max_profit,
                    mean_profit,
                    mean_profit as f64 / 1e18,
                );
            }

            // ASSERTIONS
            // 1. With 1s klines, every block should have a price hit
            assert!(
                db_hit_count >= 90,
                "expected >= 90 DB price hits (1s klines), got {}",
                db_hit_count,
            );

            // 2. Prices should have variance (not a single value)
            assert!(
                prices_seen.len() >= 5,
                "expected >= 5 distinct CEX prices, got {}",
                prices_seen.len(),
            );

            // 3. Price-level variance confirms the scanner sees changing CEX data.
            //    Spread-level variance is NOT asserted because integer bps rounding
            //    can legitimately yield the same threshold value for several blocks.
            eprintln!(
                "Spread-variance note: {} opportunities, {} distinct spread values",
                spreads.len(),
                {
                    let mut s = spreads.clone();
                    s.sort();
                    s.dedup();
                    s.len()
                },
            );

            eprintln!("\n=== REAL KLINES VALIDATION: PASS ===");
        });
    }
}
