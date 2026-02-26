//! Uniswap V3 price reader — extensibility proof.
//!
//! Reads `slot0()` data from any Uniswap V3 pool at any historical block
//! and converts `sqrtPriceX96` to a human-readable price using integer math.
//!
//! ## Scope
//!
//! This module proves V3 extensibility for the MEV backtesting toolkit.
//! It reads prices but does **not** simulate V3 swaps (which require tick
//! bitmap traversal and concentrated liquidity accounting).
//!
//! ## Architecture
//!
//! V3 stores price directly as `sqrtPriceX96` (a Q64.96 fixed-point √P),
//! unlike V2 which stores reserves and derives price. A single `slot0()`
//! call gives the current price at any block — no reserve math needed.

pub mod price;
pub mod slot0;

pub use price::{sqrt_price_x96_to_price, PriceResult};
pub use slot0::{fetch_slot0_via_call, fetch_slot0_via_storage, Slot0Data, V3_WETH_USDC_POOL};

/// Check whether V3 support is enabled via environment variable.
///
/// # Errors
/// Returns error with a user-friendly message if `MEV_ENABLE_V3` is not set to `"1"`.
pub fn require_v3_enabled() -> eyre::Result<()> {
    if std::env::var("MEV_ENABLE_V3").unwrap_or_default() != "1" {
        eyre::bail!("V3 support is experimental. Set MEV_ENABLE_V3=1 to enable.");
    }
    Ok(())
}
