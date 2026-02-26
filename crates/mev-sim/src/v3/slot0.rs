//! Slot0 reader for Uniswap V3 pools.
//!
//! Provides two methods for reading a pool's `slot0` struct:
//! - [`fetch_slot0_via_call`]: ABI-encoded `eth_call` to `slot0()` — canonical, always correct.
//! - [`fetch_slot0_via_storage`]: raw `eth_getStorageAt(pool, 0)` — faster, no EVM execution.
//!
//! Both methods return a [`Slot0Data`] struct with the decoded fields.
//! The storage approach is useful for batching and cross-validation.

use alloy::primitives::{Address, U256};
use eyre::{eyre, Result};
use reqwest::Client;
use serde::Deserialize;

/// Uniswap V3 WETH/USDC 0.3% fee pool on Ethereum mainnet.
///
/// - token0 = USDC (6 decimals)
/// - token1 = WETH (18 decimals)
pub const V3_WETH_USDC_POOL: Address = {
    let bytes: [u8; 20] = [
        0x8a, 0xd5, 0x99, 0xc3, 0xa0, 0xff, 0x1d, 0xe0, 0x82, 0x01, 0x1e, 0xfd, 0xdc, 0x58, 0xf1,
        0x90, 0x8e, 0xb6, 0xe6, 0xd8,
    ];
    Address::new(bytes)
};

/// Decoded Uniswap V3 `slot0` struct.
///
/// Contains the pool's current price state. Only `sqrt_price_x96` and `tick`
/// are needed for price derivation; the remaining fields are included for
/// completeness and cross-validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot0Data {
    /// Current √P in Q64.96 fixed-point format (uint160 on-chain).
    pub sqrt_price_x96: U256,
    /// Current tick index (int24 on-chain, stored as i32).
    pub tick: i32,
    /// Index of the most recently written oracle observation.
    pub observation_index: u16,
    /// Current oracle array capacity (used entries).
    pub observation_cardinality: u16,
    /// Pending oracle array capacity (next expansion target).
    pub observation_cardinality_next: u16,
    /// Protocol fee configuration byte.
    pub fee_protocol: u8,
    /// Reentrancy guard (true = unlocked).
    pub unlocked: bool,
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

/// Helper to make a JSON-RPC call and extract the hex result string.
async fn rpc_hex_result(
    client: &Client,
    rpc_url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<String> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let response = client
        .post(rpc_url)
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

/// Fetch `slot0` via `eth_call` to the pool's `slot0()` function.
///
/// ABI selector: `0x3850c7bd` (`slot0()`)
///
/// Returns 7 ABI-encoded values (each 32 bytes = 224 bytes total):
/// `(uint160 sqrtPriceX96, int24 tick, uint16, uint16, uint16, uint8, bool)`
///
/// # Errors
/// Returns error if the RPC call fails or the response cannot be decoded.
#[tracing::instrument(skip(rpc_url))]
pub async fn fetch_slot0_via_call(
    rpc_url: &str,
    pool: Address,
    block_number: u64,
) -> Result<Slot0Data> {
    let client = Client::new();

    // slot0() selector = 0x3850c7bd
    let data = "0x3850c7bd";

    let params = serde_json::json!([
        {
            "to": format!("{pool:#x}"),
            "data": data,
        },
        format!("0x{:x}", block_number)
    ]);

    let result_hex = rpc_hex_result(&client, rpc_url, "eth_call", params).await?;
    let raw = result_hex.trim_start_matches("0x");

    // ABI encoding: 7 words × 32 bytes = 224 bytes = 448 hex chars
    if raw.len() < 448 {
        return Err(eyre!(
            "slot0() response too short: expected 448 hex chars, got {}",
            raw.len()
        ));
    }

    // Word 0 (bytes 0..32): sqrtPriceX96 (uint160, right-aligned in 32 bytes)
    let sqrt_price_x96 = U256::from_str_radix(&raw[0..64], 16)
        .map_err(|e| eyre!("failed to parse sqrtPriceX96: {}", e))?;

    // Word 1 (bytes 32..64): tick (int24, sign-extended to int256)
    let tick_u256 = U256::from_str_radix(&raw[64..128], 16)
        .map_err(|e| eyre!("failed to parse tick: {}", e))?;
    let tick = sign_extend_int256_to_i32(tick_u256);

    // Word 2 (bytes 64..96): observationIndex (uint16)
    let obs_index = u16::from_str_radix(trim_or_zero(&raw[128..192]), 16).unwrap_or(0);

    // Word 3 (bytes 96..128): observationCardinality (uint16)
    let obs_cardinality = u16::from_str_radix(trim_or_zero(&raw[192..256]), 16).unwrap_or(0);

    // Word 4 (bytes 128..160): observationCardinalityNext (uint16)
    let obs_cardinality_next = u16::from_str_radix(trim_or_zero(&raw[256..320]), 16).unwrap_or(0);

    // Word 5 (bytes 160..192): feeProtocol (uint8)
    let fee_protocol = u8::from_str_radix(trim_or_zero(&raw[320..384]), 16).unwrap_or(0);

    // Word 6 (bytes 192..224): unlocked (bool)
    let unlocked_raw = u8::from_str_radix(trim_or_zero(&raw[384..448]), 16).unwrap_or(0);
    let unlocked = unlocked_raw != 0;

    Ok(Slot0Data {
        sqrt_price_x96,
        tick,
        observation_index: obs_index,
        observation_cardinality: obs_cardinality,
        observation_cardinality_next: obs_cardinality_next,
        fee_protocol,
        unlocked,
    })
}

/// Fetch `slot0` via `eth_getStorageAt(pool, 0)`.
///
/// Reads raw storage slot 0 (32 bytes) and unpacks the Solidity struct.
///
/// Storage layout (little-endian packed):
/// - bits \[0:160\]   → `sqrtPriceX96` (uint160)
/// - bits \[160:184\] → `tick` (int24, sign-extend from 24 bits)
/// - bits \[184:200\] → `observationIndex` (uint16)
/// - bits \[200:216\] → `observationCardinality` (uint16)
/// - bits \[216:232\] → `observationCardinalityNext` (uint16)
/// - bits \[232:240\] → `feeProtocol` (uint8)
/// - bits \[240:248\] → `unlocked` (bool, uint8)
///
/// # Errors
/// Returns error if the RPC call fails or the response cannot be decoded.
#[tracing::instrument(skip(rpc_url))]
pub async fn fetch_slot0_via_storage(
    rpc_url: &str,
    pool: Address,
    block_number: u64,
) -> Result<Slot0Data> {
    let client = Client::new();

    let params = serde_json::json!([format!("{pool:#x}"), "0x0", format!("0x{:x}", block_number)]);

    let result_hex = rpc_hex_result(&client, rpc_url, "eth_getStorageAt", params).await?;
    let raw_slot = U256::from_str_radix(result_hex.trim_start_matches("0x"), 16)
        .map_err(|e| eyre!("failed to parse storage slot 0: {}", e))?;

    // Extract sqrtPriceX96: bits [0:160] — lowest 160 bits
    let mask_160: U256 = (U256::from(1u64) << 160) - U256::from(1u64);
    let sqrt_price_x96: U256 = raw_slot & mask_160;

    // Extract tick: bits [160:184] — 24 bits, sign-extend from int24
    let shifted_160: U256 = raw_slot >> 160;
    let tick_raw: u32 = (shifted_160 & U256::from(0xFF_FFFFu32)).to::<u32>();
    let tick = sign_extend_i24(tick_raw);

    // Extract observationIndex: bits [184:200] — 16 bits
    let shifted_184: U256 = raw_slot >> 184;
    let observation_index: u16 = (shifted_184 & U256::from(0xFFFFu32)).to::<u16>();

    // Extract observationCardinality: bits [200:216] — 16 bits
    let shifted_200: U256 = raw_slot >> 200;
    let observation_cardinality: u16 = (shifted_200 & U256::from(0xFFFFu32)).to::<u16>();

    // Extract observationCardinalityNext: bits [216:232] — 16 bits
    let shifted_216: U256 = raw_slot >> 216;
    let observation_cardinality_next: u16 = (shifted_216 & U256::from(0xFFFFu32)).to::<u16>();

    // Extract feeProtocol: bits [232:240] — 8 bits
    let shifted_232: U256 = raw_slot >> 232;
    let fee_protocol: u8 = (shifted_232 & U256::from(0xFFu32)).to::<u8>();

    // Extract unlocked: bits [240:248] — 8 bits (bool)
    let shifted_240: U256 = raw_slot >> 240;
    let unlocked: bool = (shifted_240 & U256::from(0xFFu32)).to::<u8>() != 0;

    Ok(Slot0Data {
        sqrt_price_x96,
        tick,
        observation_index,
        observation_cardinality,
        observation_cardinality_next,
        fee_protocol,
        unlocked,
    })
}

/// Sign-extend a 24-bit `int24` value (stored as u32) to `i32`.
///
/// If bit 23 is set, the value is negative in two's complement.
fn sign_extend_i24(raw: u32) -> i32 {
    if raw & 0x80_0000 != 0 {
        // Negative: set upper 8 bits
        (raw | 0xFF00_0000) as i32
    } else {
        raw as i32
    }
}

/// Sign-extend a Solidity `int256` (stored as U256) to `i32`.
///
/// ABI-encoded int24 values are sign-extended to 256 bits. If the high bit
/// is set, the value is a large positive U256 representing a negative i32.
fn sign_extend_int256_to_i32(val: U256) -> i32 {
    // Check if the value is negative (high bit set in 256-bit)
    let high_bit = U256::from(1u64) << 255;
    if val & high_bit != U256::ZERO {
        // Negative: take lowest 32 bits and interpret as i32
        let low_32 = (val & U256::from(u32::MAX)).to::<u32>();
        low_32 as i32
    } else {
        // Positive: just take lowest 32 bits
        let low_32 = (val & U256::from(u32::MAX)).to::<u32>();
        low_32 as i32
    }
}

/// Trim leading zeros from a hex string, returning `"0"` for all-zero input.
fn trim_or_zero(hex: &str) -> &str {
    let trimmed = hex.trim_start_matches('0');
    if trimmed.is_empty() {
        "0"
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extend_i24_positive() {
        // tick = 100 → stored as 0x000064
        assert_eq!(sign_extend_i24(100), 100);
    }

    #[test]
    fn sign_extend_i24_negative() {
        // tick = -100 → stored as 0xFF_FF9C (24-bit two's complement)
        let raw = 0xFF_FF9Cu32 & 0xFF_FFFF;
        assert_eq!(sign_extend_i24(raw), -100);
    }

    #[test]
    fn sign_extend_i24_zero() {
        assert_eq!(sign_extend_i24(0), 0);
    }

    #[test]
    fn sign_extend_i24_max_positive() {
        // Max positive int24: 2^23 - 1 = 8388607
        assert_eq!(sign_extend_i24(0x7F_FFFF), 8_388_607);
    }

    #[test]
    fn sign_extend_i24_min_negative() {
        // Min negative int24: -2^23 = -8388608, stored as 0x800000
        assert_eq!(sign_extend_i24(0x80_0000), -8_388_608);
    }

    #[test]
    fn sign_extend_int256_positive() {
        let val = U256::from(201234u32);
        assert_eq!(sign_extend_int256_to_i32(val), 201234);
    }

    #[test]
    fn sign_extend_int256_negative() {
        // -201234 in 256-bit two's complement
        // = 2^256 - 201234
        let max = U256::MAX;
        let val = max - U256::from(201233u32); // MAX - (201234 - 1) = 2^256 - 201234
        assert_eq!(sign_extend_int256_to_i32(val), -201234);
    }

    #[test]
    fn trim_or_zero_works() {
        assert_eq!(trim_or_zero("00001a"), "1a");
        assert_eq!(trim_or_zero("000000"), "0");
        assert_eq!(trim_or_zero("ff"), "ff");
    }
}
