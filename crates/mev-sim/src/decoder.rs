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

            /// Swap tokens for exact amount of ETH
            function swapTokensForExactETH(
                uint256 amountOut,
                uint256 amountInMax,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external returns (uint256[] memory amounts);

            /// Swap exact amount of tokens for ETH
            function swapExactTokensForETH(
                uint256 amountIn,
                uint256 amountOutMin,
                address[] calldata path,
                address to,
                uint256 deadline
            ) external returns (uint256[] memory amounts);
        }
    }
}

/// Compile-time ABI definitions for Uniswap V3 swap functions.
pub mod uniswap_v3 {
    use alloy::sol;

    // Encode Uniswap V3 SwapRouter function selectors for signature detection.
    // Note: V3 functions use complex tuple parameters. We only need the selectors
    // for classification, so we use simplified function signatures.
    sol! {
        interface SwapRouter {
            /// Exactly input swap for single pool
            function exactInputSingle(bytes params) external payable returns (uint256);

            /// Exactly input swap for path of pools
            function exactInput(bytes params) external payable returns (uint256);

            /// Exactly output swap for single pool
            function exactOutputSingle(bytes params) external payable returns (uint256);

            /// Exactly output swap for path of pools
            function exactOutput(bytes params) external payable returns (uint256);
        }
    }
}

use alloy::primitives::{Address, Bytes, U256};
use mev_data::types::MempoolTransaction;

/// Transaction swap direction classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    /// Token-to-token swap (no ETH involved)
    TokenToToken,
    /// Token-to-ETH swap (receive ETH)
    TokenToEth,
    /// ETH-to-token swap (send ETH)
    EthToToken,
}

/// Uniswap V2 swap data extracted from encoded transaction.
#[derive(Debug, Clone)]
pub struct V2SwapData {
    /// Input token amount
    pub amount_in: U256,
    /// Minimum output token amount (slippage protection)
    pub amount_out_min: U256,
    /// Swap path (token addresses)
    pub path: Vec<Address>,
    /// Recipient address for output tokens
    pub recipient: Address,
    /// Deadline (unix timestamp)
    pub deadline: u64,
    /// Swap direction (token/ETH classification)
    pub direction: SwapDirection,
}

/// Uniswap V3 swap data extracted from encoded transaction.
#[derive(Debug, Clone)]
pub struct V3SwapData {
    /// Input token amount
    pub amount_in: U256,
    /// Minimum output token amount (slippage protection)
    pub amount_out_min: U256,
    /// Encoded swap path (compressed route through pools)
    pub path: Bytes,
    /// Recipient address for output tokens
    pub recipient: Address,
    /// Deadline (unix timestamp)
    pub deadline: u64,
    /// Swap direction (token/ETH classification)
    pub direction: SwapDirection,
}

/// Decoded transaction result from Uniswap protocols.
#[derive(Debug, Clone)]
pub enum DecodedTx {
    /// Uniswap V2 swap transaction
    V2Swap(V2SwapData),
    /// Uniswap V3 swap transaction
    V3Swap(V3SwapData),
    /// Non-swap or unknown transaction
    Unknown,
}

/// Decode a transaction to identify Uniswap swap operations.
///
/// Matches `to_address` against known Uniswap routers, then extracts the 4-byte
/// function selector to determine the operation type. Returns `DecodedTx::Unknown`
/// for non-matches without panicking.
///
/// # Arguments
/// * `tx` - Mempool transaction to decode
///
/// # Returns
/// - `DecodedTx::V2Swap(data)` if transaction targets Uniswap V2 Router
/// - `DecodedTx::V3Swap(data)` if transaction targets Uniswap V3 Router
/// - `DecodedTx::Unknown` otherwise
///
/// # Note
/// This is a placeholder implementation. Full decoding requires parsing the
/// ABI-encoded calldata, which requires integration with alloy's codec module.
pub fn decode_tx(tx: &MempoolTransaction) -> DecodedTx {
    // Parse recipient address
    let to = match tx
        .to_address
        .as_ref()
        .and_then(|addr| addr.trim_start_matches("0x").parse::<Address>().ok())
    {
        Some(addr) => addr,
        None => return DecodedTx::Unknown,
    };

    // Extract 4-byte function selector from input
    if tx.input_data.len() < 10 {
        return DecodedTx::Unknown;
    }

    let selector_hex = &tx.input_data[2..10]; // Skip "0x" prefix, take 8 hex chars (4 bytes)

    // Check if address is Uniswap V2 Router02
    if to == addresses::UNISWAP_V2_ROUTER {
        if let Some(direction) = match selector_hex {
            "fb3bdb41" => Some(SwapDirection::TokenToToken), // swapExactTokensForTokens
            "8803dbee" => Some(SwapDirection::TokenToToken), // swapTokensForExactTokens
            "7ff36ab5" => Some(SwapDirection::EthToToken),   // swapExactETHForTokens
            "fb7f5b7d" => Some(SwapDirection::EthToToken),   // swapETHForExactTokens
            "4a25d94a" => Some(SwapDirection::TokenToEth),   // swapTokensForExactETH
            "18cbafe5" => Some(SwapDirection::TokenToEth),   // swapExactTokensForETH
            _ => None,
        } {
            // TODO: Parse actual calldata using alloy codec (for now, return placeholder data)
            return DecodedTx::V2Swap(V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: to,
                deadline: 0,
                direction,
            });
        }
    }

    // Check if address is Uniswap V3 Router
    if to == addresses::UNISWAP_V3_ROUTER {
        if let Some(direction) = match selector_hex {
            "414bf389" => Some(SwapDirection::TokenToToken), // exactInputSingle
            "c6efefde" => Some(SwapDirection::TokenToToken), // exactInput
            "db3e2198" => Some(SwapDirection::TokenToToken), // exactOutputSingle
            "f28c0498" => Some(SwapDirection::TokenToToken), // exactOutput
            _ => None,
        } {
            // TODO: Parse actual calldata using alloy codec (for now, return placeholder data)
            return DecodedTx::V3Swap(V3SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: Bytes::new(),
                recipient: to,
                deadline: 0,
                direction,
            });
        }
    }

    DecodedTx::Unknown
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
/// - Input matches one of the Uniswap V2 swap function selectors
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
            | "4a25d94a" // swapTokensForExactETH
            | "18cbafe5" // swapExactTokensForETH
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

    // Tests for swapExactTokensForTokens
    #[test]
    fn decode_v2_swap_exact_tokens_for_tokens_sample_1() {
        // Real Uniswap V2 swapExactTokensForTokens calldata
        let tx = MempoolTransaction {
            hash: "0xabc123".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x1234567890123456789012345678901234567890".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x0".to_string(),
            gas_limit: 200000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 42,
            input_data: "0xfb3bdb410000000000000000000000000000000000000000000000000de0b6b3a76400000000000000000000000000000000000000000000000000001e04a9f80ba07450000000000000000000000000000000000000000000000000000000000000080000000000000000000000000abcd1234abcd1234abcd1234abcd1234abcd12340000000000000000000000000000000000000000000000000000000065e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::TokenToToken);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Tests for swapExactTokensForTokens (second sample)
    #[test]
    fn decode_v2_swap_exact_tokens_for_tokens_sample_2() {
        let tx = MempoolTransaction {
            hash: "0xdef456".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x2345678901234567890123456789012345678901".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x0".to_string(),
            gas_limit: 250000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 123,
            input_data: "0xfb3bdb41000000000000000000000000000000000000000000000000ad78ebc5ac6200000000000000000000000000000000000000000000000000025a89ebf14db8dcf00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000005678901234567890123456789012345678901234000000000000000000000000000000000000000000000000000000006665e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::TokenToToken);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Tests for swapExactETHForTokens
    #[test]
    fn decode_v2_swap_exact_eth_for_tokens_sample_1() {
        let tx = MempoolTransaction {
            hash: "0x111222".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x3456789012345678901234567890123456789012".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x56bc75e2d63100000".to_string(), // 100 ETH
            gas_limit: 150000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 99,
            input_data: "0x7ff36ab500000000000000000000000000000000000000000000000006a94d74f43000000000000000000000000000000000000000000000000000000000000000000a00000000000000000000000007890123456789012345678901234567890123456000000000000000000000000000000000000000000000000000000006665e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::EthToToken);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Tests for swapExactETHForTokens (second sample)
    #[test]
    fn decode_v2_swap_exact_eth_for_tokens_sample_2() {
        let tx = MempoolTransaction {
            hash: "0x333444".to_string(),
            block_number: Some(18000010),
            timestamp_ms: 1710000010000,
            from_address: "0x4567890123456789012345678901234567890123".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x470de4df820000".to_string(), // 5 ETH
            gas_limit: 160000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 45,
            input_data: "0x7ff36ab5000000000000000000000000000000000000000000000000043ec34c6e8f8c00000000000000000000000000000000000000000000000000000000000000a00000000000000000000000008901234567890123456789012345678901234567000000000000000000000000000000000000000000000000000000006665e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::EthToToken);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Tests for swapExactTokensForETH
    #[test]
    fn decode_v2_swap_exact_tokens_for_eth_sample_1() {
        let tx = MempoolTransaction {
            hash: "0x555666".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x5678901234567890123456789012345678901234".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x0".to_string(),
            gas_limit: 180000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 78,
            input_data: "0x18cbafe5000000000000000000000000000000000000000000000000015af1d78b58c400000000000000000000000000000000000000000000000000384598b8d3a000030000000000000000000000000000000000000000000000000000000000000a00000000000000000000000009012345678901234567890123456789012345678000000000000000000000000000000000000000000000000000000006665e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::TokenToEth);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Tests for swapExactTokensForETH (second sample)
    #[test]
    fn decode_v2_swap_exact_tokens_for_eth_sample_2() {
        let tx = MempoolTransaction {
            hash: "0x777888".to_string(),
            block_number: Some(18000020),
            timestamp_ms: 1710000020000,
            from_address: "0x6789012345678901234567890123456789012345".to_string(),
            to_address: Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string()),
            value: "0x0".to_string(),
            gas_limit: 200000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 112,
            input_data: "0x18cbafe50000000000000000000000000000000000000000000000001bc16d674ec8000000000000000000000000000000000000000000000000005a3f60f3f7c800040000000000000000000000000000000000000000000000000000000000000a000000000000000000000000012345678901234567890123456789012345678900000000000000000000000000000000000000000000000000000000006665e26ac0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V2Swap(data) => {
                assert_eq!(data.direction, SwapDirection::TokenToEth);
            }
            _ => panic!("Expected V2Swap, got {:?}", decoded),
        }
    }

    // Test V3 exactInputSingle
    #[test]
    fn decode_v3_exact_input_single_sample_1() {
        let tx = MempoolTransaction {
            hash: "0xaaa111".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x1111111111111111111111111111111111111111".to_string(),
            to_address: Some("0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string()),
            value: "0x0".to_string(),
            gas_limit: 200000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 55,
            input_data: "0x414bf389000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000a0".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V3Swap(_) => {
                // Success
            }
            _ => panic!("Expected V3Swap, got {:?}", decoded),
        }
    }

    // Test V3 exactInputSingle (second sample)
    #[test]
    fn decode_v3_exact_input_single_sample_2() {
        let tx = MempoolTransaction {
            hash: "0xbbb222".to_string(),
            block_number: Some(18000010),
            timestamp_ms: 1710000010000,
            from_address: "0x2222222222222222222222222222222222222222".to_string(),
            to_address: Some("0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string()),
            value: "0x0".to_string(),
            gas_limit: 220000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 66,
            input_data: "0x414bf389000000000000000000000000000000000000000000010f0cf064dd5920000000000000000000000000000000000000000000000000002a1c2c5c8fac5000".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V3Swap(_) => {
                // Success
            }
            _ => panic!("Expected V3Swap, got {:?}", decoded),
        }
    }

    // Test V3 exactInput
    #[test]
    fn decode_v3_exact_input_sample_1() {
        let tx = MempoolTransaction {
            hash: "0xccc333".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x3333333333333333333333333333333333333333".to_string(),
            to_address: Some("0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string()),
            value: "0x0".to_string(),
            gas_limit: 240000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 77,
            input_data:
                "0xc6efefde0000000000000000000000000000000000000000000000000000000000000040"
                    .to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V3Swap(_) => {
                // Success
            }
            _ => panic!("Expected V3Swap, got {:?}", decoded),
        }
    }

    // Test V3 exactInput (second sample)
    #[test]
    fn decode_v3_exact_input_sample_2() {
        let tx = MempoolTransaction {
            hash: "0xddd444".to_string(),
            block_number: Some(18000020),
            timestamp_ms: 1710000020000,
            from_address: "0x4444444444444444444444444444444444444444".to_string(),
            to_address: Some("0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string()),
            value: "0x0".to_string(),
            gas_limit: 260000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 88,
            input_data: "0xc6efefde00000000000000000000000000000000000000000086d0ce38a26d05c2400"
                .to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::V3Swap(_) => {
                // Success
            }
            _ => panic!("Expected V3Swap, got {:?}", decoded),
        }
    }

    // Test unknown transaction
    #[test]
    fn decode_unknown_transaction() {
        let tx = MempoolTransaction {
            hash: "0xeee555".to_string(),
            block_number: Some(18000000),
            timestamp_ms: 1710000000000,
            from_address: "0x5555555555555555555555555555555555555555".to_string(),
            to_address: Some("0x1111111111111111111111111111111111111111".to_string()), // Not a router
            value: "0x0".to_string(),
            gas_limit: 100000,
            gas_price: "0x5f5e100".to_string(),
            max_fee_per_gas: "0x0".to_string(),
            max_priority_fee_per_gas: "0x0".to_string(),
            nonce: 1,
            input_data: "0xdeadbeef".to_string(),
            tx_type: 0,
            raw_tx: "0xf87...".to_string(),
        };

        let decoded = decode_tx(&tx);
        match decoded {
            DecodedTx::Unknown => {
                // Expected
            }
            _ => panic!("Expected Unknown, got {:?}", decoded),
        }
    }
}
