//! Uniswap transaction decoder using compile-time ABI definitions.
//!
//! This module provides statically-compiled function signature decoders for Uniswap V2 and later.
//! Using `alloy::sol!` macros instead of runtime JSON ABI parsing offers:
//!
//! **Why compile-time sol! over runtime ABI?**
//! 1. **Zero overhead**: Function selectors computed at compile time, not loaded from JSON files
//! 2. **Type safety**: Strongly-typed decode results with Rust compiler guarantees
//! 3. **Auditability**: ABI embedded in source code, reviewable and versionable
//! 4. **No file I/O**: No dependency on external ABI files or network fetches
//! 5. **Consistent versioning**: Use only canonical signatures; avoid runtime ABI mismatches
//!
//! Trade-off: Limited to hardcoded function signatures. New DEX protocols require code recompilation.

/// Ethereum mainnet contract addresses (compile-time constants).
pub mod addresses {
    use alloy::primitives::Address;

    /// Uniswap V2 Router02 — primary entry point for swaps on V2
    pub const UNISWAP_V2_ROUTER: Address =
        alloy::primitives::address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");

    /// Uniswap V2 Factory — pool creation and lookups
    pub const UNISWAP_V2_FACTORY: Address =
        alloy::primitives::address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");

    /// Uniswap V3 Router — swap and liquidity routing
    pub const UNISWAP_V3_ROUTER: Address =
        alloy::primitives::address!("E592427A0AEce92De3Edee1F18E0157C05861564");

    /// Uniswap V3 Factory — pool registry
    pub const UNISWAP_V3_FACTORY: Address =
        alloy::primitives::address!("1F98431c8aD98523631AE4a59f267346ea31F984");

    /// Wrapped Ether (WETH) on mainnet
    pub const WETH: Address =
        alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

    /// USD Coin (USDC) on mainnet
    pub const USDC: Address =
        alloy::primitives::address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    /// Tether (USDT) on mainnet
    pub const USDT: Address =
        alloy::primitives::address!("dAC17F958D2ee523a2206206994597C13D831ec7");

    /// Dai Stablecoin (DAI) on mainnet
    pub const DAI: Address =
        alloy::primitives::address!("6B175474E89094C44Da98b954EedeAC495271d0F");

    /// Wrapped Bitcoin (WBTC) on mainnet
    pub const WBTC: Address =
        alloy::primitives::address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
}

/// Compile-time ABI definitions for Uniswap V2 swap functions.
///
/// The `sol!` macro generates type-safe function selectors and decode support
/// at compile time without requiring runtime JSON parsing.
pub mod uniswap_v2 {
    use alloy::sol;

    // Encode Uniswap V2 Router02 function signatures
    sol! {
        interface UniswapV2Router02 {
            /// Swap exact amount of input tokens for output tokens
            function swapExactTokensForTokens(
                uint256 amountIn,
                uint256 amountOutMin,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external returns (uint256[] memory amounts);

            /// Swap input tokens for exact amount of output tokens
            function swapTokensForExactTokens(
                uint256 amountOut,
                uint256 amountInMax,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external returns (uint256[] memory amounts);

            /// Swap exact amount of ETH for output tokens
            function swapExactETHForTokens(
                uint256 amountOutMin,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external payable returns (uint256[] memory amounts);

            /// Swap ETH for exact amount of output tokens
            function swapETHForExactTokens(
                uint256 amountOut,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external payable returns (uint256[] memory amounts);
        }
    }
}

/// Check if a transaction targets Uniswap V2 and invokes a swap.
///
/// # Arguments
/// * `to_address` - Recipient address (as lowercase hex string with 0x prefix)
/// * `input` - Transaction input data (as hex string with 0x prefix)
///
/// # Returns
/// `true` if:
/// - `to_address` is the Uniswap V2 Router02
/// - Input matches one of the four Uniswap V2 swap function selectors
///
/// # Example
/// ```ignore
/// let is_swap = is_v2_swap(
///     "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
///     "0xfb3bdb41..." // swapExactTokensForTokens selector
/// );
/// ```
pub fn is_v2_swap(to_address: &str, input: &str) -> bool {
    // Parse recipient address
    let to = match to_address
        .trim_start_matches("0x")
        .parse::<alloy::primitives::Address>()
    {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    // Check if address is Uniswap V2 Router02
    if to != addresses::UNISWAP_V2_ROUTER {
        return false;
    }

    // Extract 4-byte function selector from input
    if input.len() < 10 {
        // Must have at least "0x" + 8 hex chars for selector
        return false;
    }

    let selector_hex = &input[2..10]; // Skip "0x" prefix, take 8 chars (4 bytes)

    // Compare against known Uniswap V2 swap selectors
    // These are the first 4 bytes of each function's Keccak256 hash
    matches!(
        selector_hex,
        "fb3bdb41" // swapExactTokensForTokens
            | "8803dbee" // swapTokensForExactTokens
            | "7ff36ab5" // swapExactETHForTokens
            | "fb7f5b7d" // swapETHForExactTokens
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_valid() {
        assert_eq!(
            addresses::UNISWAP_V2_ROUTER.to_checksum(None),
            "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"
        );
    }

    #[test]
    fn is_v2_swap_recognizes_valid_swap() {
        let result = is_v2_swap(
            "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D",
            "0xfb3bdb41", // swapExactTokensForTokens selector
        );
        assert!(result);
    }

    #[test]
    fn is_v2_swap_rejects_wrong_router() {
        let result = is_v2_swap("0x1111111111111111111111111111111111111111", "0xfb3bdb41");
        assert!(!result);
    }

    #[test]
    fn is_v2_swap_rejects_wrong_selector() {
        let result = is_v2_swap("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D", "0xdeadbeef");
        assert!(!result);
    }

    #[test]
    fn is_v2_swap_handles_short_input() {
        let result = is_v2_swap("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D", "0x");
        assert!(!result);
    }
}
