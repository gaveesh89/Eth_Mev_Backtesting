//! Integer-only price conversion from Uniswap V3 `sqrtPriceX96`.
//!
//! ## Math
//!
//! `sqrtPriceX96` encodes $\sqrt{P} \times 2^{96}$ where $P = \frac{token1}{token0}$
//! in raw units. To recover a human-readable price:
//!
//! $$P_{raw} = \frac{sqrtPriceX96^2}{2^{192}}$$
//!
//! Then adjust for decimal difference between the two tokens.
//!
//! ## Overflow Handling
//!
//! `sqrtPriceX96` is `uint160`, so `sqrtPriceX96²` can be up to 320 bits —
//! exceeding U256's 256-bit capacity. We right-shift by 32 first, squaring
//! the result (max 256 bits), then adjust the denominator from $2^{192}$
//! to $2^{128}$. This loses 32 bits of precision in the square root,
//! leaving ~128 bits ≈ 38 decimal digits — more than sufficient for price display.
//!
//! ## No f64 in Computation
//!
//! All math uses `U256`. The `f64` type is used **only** in the final
//! `display` string formatting, never in the computation path.

use alloy::primitives::U256;

/// Result of converting `sqrtPriceX96` to a human-readable price.
#[derive(Debug, Clone)]
pub struct PriceResult {
    /// Price scaled by `10^precision_decimals`.
    /// For example, if price is 2950.42 and precision is 8, this is 295_042_000_000.
    pub price_scaled: U256,
    /// Number of decimal places in `price_scaled`.
    pub precision_decimals: u8,
    /// Human-readable price string (e.g., "2950.42"). `f64` is used here only.
    pub display: String,
}

/// Precision: 8 decimal places in the scaled integer price.
const PRECISION: u8 = 8;

/// Convert `sqrtPriceX96` to a human-readable price.
///
/// ## Parameters
///
/// - `sqrt_price_x96`: The raw Q64.96 value from slot0.
/// - `token0_decimals`: Number of decimals for token0 (e.g., 6 for USDC).
/// - `token1_decimals`: Number of decimals for token1 (e.g., 18 for WETH).
/// - `is_token0_quote`: If `true`, returns price denominated in token0
///   (e.g., "USDC per WETH"). If `false`, returns price in token1 terms.
///
/// ## Math (all U256)
///
/// The raw price $P = \frac{token1}{token0}$ in base units:
/// ```text
/// P_raw = sqrtPriceX96² / 2^192
/// ```
///
/// To avoid U256 overflow on squaring (uint160² can be 320 bits):
/// ```text
/// shifted = sqrtPriceX96 >> 32
/// sq = shifted * shifted          // max 256 bits
/// denominator = 2^128             // was 2^192, reduced by 2^64
/// ```
///
/// Then adjust for token decimals and requested quote direction.
pub fn sqrt_price_x96_to_price(
    sqrt_price_x96: U256,
    token0_decimals: u8,
    token1_decimals: u8,
    is_token0_quote: bool,
) -> PriceResult {
    // Shift down by 32 to keep within U256 after squaring.
    // sqrtPriceX96 is uint160, so shifted is at most 128 bits,
    // and shifted² is at most 256 bits.
    let shifted: U256 = sqrt_price_x96 >> 32;

    // If shifted is zero (extremely low price), return zero result
    if shifted.is_zero() {
        return PriceResult {
            price_scaled: U256::ZERO,
            precision_decimals: PRECISION,
            display: "0.00".to_string(),
        };
    }

    let sq = shifted * shifted;

    // Denominator was 2^192, but we shifted the input by 2^32, so we
    // shifted the square by 2^64. New denominator: 2^(192-64) = 2^128.
    let denom_shift: u32 = 128;

    // Compute 10^precision for scaling
    let scale = U256::from(10u64).pow(U256::from(PRECISION));

    if is_token0_quote {
        // Price in token0 per token1 = token0_per_token1
        // Raw: P = sqrtPriceX96² / 2^192 gives token1/token0 in base units
        // We want token0/token1 = 1/P
        //
        // token0_per_token1 = 2^192 / sqrtPriceX96² × 10^(token1_decimals - token0_decimals)
        //
        // Using shifted: = 2^128 / sq × 10^(token1_dec - token0_dec)
        //
        // With precision scaling:
        //   price_scaled = (2^128 × 10^precision × 10^(token1_dec - token0_dec)) / sq

        let decimal_adjustment = if token1_decimals >= token0_decimals {
            U256::from(10u64).pow(U256::from(token1_decimals - token0_decimals))
        } else {
            U256::from(1u64)
        };

        // numerator = 2^128 × scale × decimal_adjustment
        let numerator = (U256::from(1u64) << denom_shift) * scale * decimal_adjustment;

        let price_scaled = if token1_decimals < token0_decimals {
            let divisor = U256::from(10u64).pow(U256::from(token0_decimals - token1_decimals));
            numerator / (sq * divisor)
        } else {
            numerator / sq
        };

        let display = format_price_display(price_scaled, PRECISION);

        PriceResult {
            price_scaled,
            precision_decimals: PRECISION,
            display,
        }
    } else {
        // Price in token1 per token0 = P directly
        // Raw: P = sqrtPriceX96² / 2^192 = sq / 2^128 (using shifted)
        //
        // Adjust for decimals:
        //   price = sq × 10^(token0_dec - token1_dec) / 2^128
        //
        // With precision:
        //   price_scaled = sq × 10^precision × 10^(token0_dec - token1_dec) / 2^128

        let decimal_adjustment = if token0_decimals >= token1_decimals {
            U256::from(10u64).pow(U256::from(token0_decimals - token1_decimals))
        } else {
            U256::from(1u64)
        };

        let numerator = sq * scale * decimal_adjustment;

        let price_scaled = if token0_decimals < token1_decimals {
            let divisor = U256::from(10u64).pow(U256::from(token1_decimals - token0_decimals));
            numerator / ((U256::from(1u64) << denom_shift) * divisor)
        } else {
            numerator / (U256::from(1u64) << denom_shift)
        };

        let display = format_price_display(price_scaled, PRECISION);

        PriceResult {
            price_scaled,
            precision_decimals: PRECISION,
            display,
        }
    }
}

/// Format a scaled integer price as a decimal string.
///
/// `f64` is used only here for final display formatting.
fn format_price_display(price_scaled: U256, precision: u8) -> String {
    let divisor = U256::from(10u64).pow(U256::from(precision));
    let integer_part = price_scaled / divisor;
    let fractional_part = price_scaled % divisor;

    // Convert to strings for formatting
    let int_str = format!("{integer_part}");
    let frac_str = format!("{:0>width$}", fractional_part, width = precision as usize);

    // Trim trailing zeros for cleaner display (keep at least 2 decimals)
    let trimmed = frac_str.trim_end_matches('0');
    let frac_display = if trimmed.len() < 2 {
        &frac_str[..2]
    } else {
        trimmed
    };

    format!("{int_str}.{frac_display}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqrt_price_x96_to_price_known_value() {
        // Known value from RareSkills example: USDC/WETH pool
        // sqrtPriceX96 = 1506673274302120988651364689808458
        // Expected price: ~$2765 USDC per WETH (verified via Python)
        let sqrt_price =
            U256::from_str_radix("1506673274302120988651364689808458", 10).expect("valid U256");

        // token0 = USDC (6 dec), token1 = WETH (18 dec)
        // is_token0_quote = true → USDC per WETH
        let result = sqrt_price_x96_to_price(sqrt_price, 6, 18, true);

        // Extract integer part for sanity check
        let divisor = U256::from(10u64).pow(U256::from(result.precision_decimals));
        let integer_part: u64 = (result.price_scaled / divisor).to::<u64>();

        // Should be within range of ~$2765
        assert!(
            (2700..=2830).contains(&integer_part),
            "Expected price ~2765 USDC/WETH, got {integer_part} (display: {})",
            result.display
        );
    }

    #[test]
    fn test_sqrt_price_inverse_relationship() {
        // token0_per_token1 × token1_per_token0 should ≈ 1.0
        let sqrt_price =
            U256::from_str_radix("1506673274302120988651364689808458", 10).expect("valid U256");

        let p0 = sqrt_price_x96_to_price(sqrt_price, 6, 18, true);
        let p1 = sqrt_price_x96_to_price(sqrt_price, 6, 18, false);

        // Multiply the two scaled prices and check product ≈ 10^(2*precision)
        let product = p0.price_scaled * p1.price_scaled;
        let expected_product = U256::from(10u64).pow(U256::from(2 * PRECISION));

        // Allow 1% tolerance due to integer rounding
        let lower = expected_product * U256::from(99u64) / U256::from(100u64);
        let upper = expected_product * U256::from(101u64) / U256::from(100u64);
        assert!(
            product >= lower && product <= upper,
            "Inverse product {product} not within 1% of {expected_product}"
        );
    }

    #[test]
    fn test_zero_sqrt_price_returns_zero() {
        let result = sqrt_price_x96_to_price(U256::ZERO, 6, 18, true);
        assert_eq!(result.price_scaled, U256::ZERO);
        assert_eq!(result.display, "0.00");
    }

    #[test]
    fn test_same_decimals() {
        // If both tokens have 18 decimals, the price should be the raw ratio
        let sqrt_price = U256::from(1u64) << 96; // sqrtPriceX96 = 2^96 → P = 1.0
        let result = sqrt_price_x96_to_price(sqrt_price, 18, 18, true);

        let divisor = U256::from(10u64).pow(U256::from(result.precision_decimals));
        let integer_part: u64 = (result.price_scaled / divisor).to::<u64>();

        // P = 1.0 → inverse = 1.0
        assert_eq!(integer_part, 1, "Expected price 1 for equal reserves");
    }

    #[test]
    fn test_format_price_display() {
        // 295042000000 with 8 decimals → "2950.42"
        let scaled = U256::from(295_042_000_000u64);
        let display = format_price_display(scaled, 8);
        assert_eq!(display, "2950.42");
    }

    #[test]
    fn test_format_price_display_trailing_zeros() {
        // 100_00000000 with 8 decimals → "100.00"
        let scaled = U256::from(10_000_000_000u64);
        let display = format_price_display(scaled, 8);
        assert_eq!(display, "100.00");
    }
}
