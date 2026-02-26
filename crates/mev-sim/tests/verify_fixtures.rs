//! Etherscan-based cross-verification of Criterion 2 fixture transactions.
//!
//! The fixture `tx_hash` values are placeholders (first tx in each block).
//! This test uses Etherscan's `getLogs` API to find **all** Swap events on
//! tracked pools within each fixture's block, then checks whether cross-DEX
//! arb activity actually occurred — i.e. whether BOTH pools in a fixture pair
//! emitted Swap events in the same transaction.
//!
//! # Requirements
//! - `ETHERSCAN_API_KEY` environment variable must be set
//! - Network access to `api.etherscan.io`
//!
//! # Usage
//! ```bash
//! ETHERSCAN_API_KEY=xxx cargo test -p mev-sim verify_fixtures_on_etherscan -- --ignored --nocapture
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Uniswap V2 / SushiSwap `Swap` event topic0.
const SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";

/// ERC-20 `Transfer(address,address,uint256)` event topic0.
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// WETH contract address (lowercase, with 0x prefix).
const WETH_ADDRESS: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";

/// Tracked pool addresses (lowercase, with 0x prefix).
const TRACKED_POOLS: &[(&str, &str)] = &[
    (
        "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc",
        "UniV2_WETH_USDC",
    ),
    (
        "0x397ff1542f962076d0bfe58ea045ffa2d347aca0",
        "Sushi_WETH_USDC",
    ),
    (
        "0xa478c2975ab1ea89e8196811f51a7b7ade33eb11",
        "UniV2_WETH_DAI",
    ),
    (
        "0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f",
        "Sushi_WETH_DAI",
    ),
    (
        "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852",
        "UniV2_WETH_USDT",
    ),
    (
        "0x06da0fd433c1a5d7a4faa01111c044910a184553",
        "Sushi_WETH_USDT",
    ),
];

/// Fixture entry as stored in `known_arb_txs.json`.
///
/// Fields are a superset: the original fields plus optional verification metadata
/// that this test appends.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KnownArbTx {
    block_number: u64,
    tx_hash: String,
    tx_index: u64,
    pair: String,
    profit_approx_wei: u64,

    // --- verification metadata (added by this test) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verification_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_arb_tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_arb_tx_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    swap_details: Option<Vec<String>>,

    // --- profit verification (added by profit verification pass) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_gross_profit_wei: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_net_profit_wei: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_gas_cost_wei: Option<String>,
}

/// Etherscan API response for getLogs / proxy calls.
#[derive(Debug, Deserialize)]
struct EtherscanResponse {
    result: Option<serde_json::Value>,
    #[allow(dead_code)]
    status: Option<String>,
    #[allow(dead_code)]
    message: Option<String>,
}

/// Decoded swap direction on a single pool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SwapDetail {
    pool_address: String,
    pool_name: String,
    tx_hash: String,
    amount0_in: u128,
    amount1_in: u128,
    amount0_out: u128,
    amount1_out: u128,
}

impl std::fmt::Display for SwapDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let direction = if self.amount0_in > 0 && self.amount1_out > 0 {
            "token0->token1"
        } else if self.amount1_in > 0 && self.amount0_out > 0 {
            "token1->token0"
        } else {
            "mixed"
        };
        write!(
            f,
            "{} [{}] a0in={} a1in={} a0out={} a1out={}",
            self.pool_name,
            direction,
            self.amount0_in,
            self.amount1_in,
            self.amount0_out,
            self.amount1_out,
        )
    }
}

/// Decode the 256-hex-char `data` field of a V2 Swap event into four u128 amounts.
///
/// Layout: `amount0In (32B) | amount1In (32B) | amount0Out (32B) | amount1Out (32B)`
fn decode_swap_data(data_hex: &str) -> Option<(u128, u128, u128, u128)> {
    let hex = data_hex.strip_prefix("0x").unwrap_or(data_hex);
    if hex.len() < 256 {
        return None;
    }
    let parse_chunk = |start: usize| -> Option<u128> {
        let chunk = &hex[start..start + 64];
        let trimmed = chunk.trim_start_matches('0');
        let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
        u128::from_str_radix(trimmed, 16).ok()
    };
    Some((
        parse_chunk(0)?,
        parse_chunk(64)?,
        parse_chunk(128)?,
        parse_chunk(192)?,
    ))
}

/// Decode a 32-byte (64 hex char) `uint256` value from a Transfer event's `data` field.
///
/// Returns `None` if the hex is too long (>32 hex chars after stripping) to fit u128,
/// or if parsing fails.
fn decode_transfer_amount(data_hex: &str) -> Option<u128> {
    let hex = data_hex.strip_prefix("0x").unwrap_or(data_hex);
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    // Guard: u128 holds 32 hex chars max — ERC-20 amounts should fit
    if trimmed.len() > 32 {
        return None;
    }
    u128::from_str_radix(trimmed, 16).ok()
}

/// Extract a 20-byte address from a 32-byte indexed topic (left-zero-padded).
///
/// E.g. `"0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"`
/// → `"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"`
fn address_from_topic(topic: &str) -> Option<String> {
    let hex = topic.strip_prefix("0x").unwrap_or(topic);
    if hex.len() < 40 {
        return None;
    }
    let addr = &hex[hex.len() - 40..];
    Some(format!("0x{}", addr.to_lowercase()))
}

/// Parse a `0x`-prefixed hex string into u128 (for gasUsed, effectiveGasPrice, etc.).
fn parse_hex_u128(value: &str) -> Option<u128> {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    u128::from_str_radix(trimmed, 16).ok()
}

/// Balance-delta map: `address → (token_address → signed_balance_change)`.
///
/// Positive values = net inflow, negative = net outflow.
type BalanceDeltaMap = HashMap<String, HashMap<String, i128>>;

/// Build balance deltas from ALL Transfer logs in a receipt.
///
/// For each `Transfer(from, to, amount)`:
///   `delta[to][token]   += amount`
///   `delta[from][token] -= amount`
fn build_balance_deltas(logs: &[serde_json::Value]) -> BalanceDeltaMap {
    let mut deltas: BalanceDeltaMap = HashMap::new();

    for log in logs {
        let topics: Vec<String> = log
            .get("topics")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();

        // Must be Transfer with 3 indexed topics: topic0, from, to
        if topics.len() < 3 || topics[0] != TRANSFER_TOPIC {
            continue;
        }

        let token_address = log
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let from = match address_from_topic(&topics[1]) {
            Some(a) => a,
            None => continue,
        };
        let to = match address_from_topic(&topics[2]) {
            Some(a) => a,
            None => continue,
        };

        let data_hex = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x0");
        let amount = match decode_transfer_amount(data_hex) {
            Some(a) => a as i128,
            None => continue,
        };

        // from loses tokens
        *deltas
            .entry(from)
            .or_default()
            .entry(token_address.clone())
            .or_insert(0) -= amount;

        // to gains tokens
        *deltas
            .entry(to)
            .or_default()
            .entry(token_address)
            .or_insert(0) += amount;
    }

    deltas
}

/// Fetch a transaction receipt from Etherscan's `eth_getTransactionReceipt` proxy.
async fn fetch_receipt(
    client: &reqwest::Client,
    api_key: &str,
    tx_hash: &str,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "https://api.etherscan.io/v2/api?chainid=1&module=proxy&action=eth_getTransactionReceipt\
         &txhash={}&apikey={}",
        tx_hash, api_key,
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let body: EtherscanResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {e}"))?;

    match body.result {
        Some(v) if !v.is_null() => Ok(v),
        _ => Err("null or missing receipt".to_string()),
    }
}

/// Resolve fixture file path: `<workspace_root>/test_data/known_arb_txs.json`.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test_data")
        .join("known_arb_txs.json")
}

/// Build the tracked-pool lookup (lowercase address -> human name).
fn tracked_pool_map() -> HashMap<String, String> {
    TRACKED_POOLS
        .iter()
        .map(|(addr, name)| (addr.to_string(), name.to_string()))
        .collect()
}

/// For a given block, fetch all Swap logs on tracked pools from Etherscan `getLogs`.
/// Returns a map: tx_hash -> Vec<SwapDetail>.
async fn fetch_swap_logs_in_block(
    client: &reqwest::Client,
    api_key: &str,
    block_number: u64,
    pool_map: &HashMap<String, String>,
) -> Result<HashMap<String, Vec<SwapDetail>>, String> {
    // Etherscan V2 getLogs API — uses decimal block numbers and chainid=1
    let url = format!(
        "https://api.etherscan.io/v2/api?chainid=1&module=logs&action=getLogs\
         &fromBlock={bn}&toBlock={bn}\
         &topic0={topic}\
         &apikey={key}",
        bn = block_number,
        topic = SWAP_TOPIC,
        key = api_key,
    );

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let body: EtherscanResponse = response
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let logs = match body.result {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(serde_json::Value::String(ref s)) if s == "Max rate limit reached" => {
            return Err("Etherscan rate limit hit".to_string());
        }
        _ => return Ok(HashMap::new()),
    };

    let mut by_tx: HashMap<String, Vec<SwapDetail>> = HashMap::new();

    for log in &logs {
        let log_address = log
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        // Only keep logs from our tracked pools
        let pool_name = match pool_map.get(&log_address) {
            Some(n) => n.clone(),
            None => continue,
        };

        let tx_hash = log
            .get("transactionHash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        let data_hex = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x");

        let (a0in, a1in, a0out, a1out) = decode_swap_data(data_hex).unwrap_or((0, 0, 0, 0));

        by_tx.entry(tx_hash.clone()).or_default().push(SwapDetail {
            pool_address: log_address,
            pool_name,
            tx_hash,
            amount0_in: a0in,
            amount1_in: a1in,
            amount0_out: a0out,
            amount1_out: a1out,
        });
    }

    Ok(by_tx)
}

#[tokio::test]
#[ignore = "requires ETHERSCAN_API_KEY and network access"]
async fn verify_fixtures_on_etherscan() {
    // ---- prerequisites ----
    let api_key = match std::env::var("ETHERSCAN_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("ETHERSCAN_API_KEY not set; skipping fixture verification");
            return;
        }
    };

    let path = fixture_path();
    if !path.exists() {
        eprintln!("Fixture file not found at {}; skipping", path.display());
        return;
    }

    let raw = std::fs::read_to_string(&path).expect("known_arb_txs.json should be readable");
    let mut fixtures: Vec<KnownArbTx> =
        serde_json::from_str(&raw).expect("known_arb_txs.json must be valid JSON array");

    if fixtures.is_empty() {
        eprintln!("known_arb_txs.json is empty; nothing to verify");
        return;
    }

    let pool_map = tracked_pool_map();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client should build");

    // ---- Phase 1: Fetch Swap logs for each unique block ----
    let unique_blocks: Vec<u64> = {
        let mut seen = HashSet::new();
        fixtures
            .iter()
            .filter_map(|f| {
                if seen.insert(f.block_number) {
                    Some(f.block_number)
                } else {
                    None
                }
            })
            .collect()
    };

    // block_number -> (tx_hash -> Vec<SwapDetail>)
    let mut block_swaps: HashMap<u64, HashMap<String, Vec<SwapDetail>>> = HashMap::new();

    eprintln!(
        "\n=== Fetching Swap logs for {} unique blocks via Etherscan getLogs ===\n",
        unique_blocks.len()
    );

    for block in &unique_blocks {
        // Rate limit
        tokio::time::sleep(Duration::from_millis(250)).await;

        eprintln!("  Block {} ...", block);

        match fetch_swap_logs_in_block(&client, &api_key, *block, &pool_map).await {
            Ok(swaps) => {
                let total_swaps: usize = swaps.values().map(|v| v.len()).sum();
                let tx_count = swaps.len();
                eprintln!(
                    "    Found {} Swap events across {} transactions on tracked pools",
                    total_swaps, tx_count
                );
                block_swaps.insert(*block, swaps);
            }
            Err(e) => {
                eprintln!(
                    "    WARNING: failed to fetch logs for block {}: {}",
                    block, e
                );
                block_swaps.insert(*block, HashMap::new());
            }
        }
    }

    // ---- Phase 2: For each fixture entry, find cross-DEX arb txs ----
    eprintln!("\n=== Verifying {} fixture entries ===\n", fixtures.len());

    let mut confirmed_count: usize = 0;
    let mut opportunity_only_count: usize = 0;

    // Header
    eprintln!(
        "{:<10} | {:<32} | {:<28} | {:<8} | {:<50} | swap_details",
        "block", "fixture_pair", "status", "arb_txs", "actual_arb_tx"
    );
    eprintln!("{}", "-".repeat(170));

    for fixture in &mut fixtures {
        let swaps_in_block = block_swaps
            .get(&fixture.block_number)
            .cloned()
            .unwrap_or_default();

        // Extract the two pool addresses from the fixture pair
        let fixture_pool_a = fixture.pair.split('/').next().unwrap_or("").to_lowercase();
        let fixture_pool_b = fixture.pair.split('/').nth(1).unwrap_or("").to_lowercase();

        // Find transactions that touch BOTH pools in this pair
        let mut cross_dex_txs: Vec<(String, Vec<SwapDetail>)> = Vec::new();
        // Also find txs that touch at least one
        let mut any_pool_txs: Vec<(String, Vec<SwapDetail>)> = Vec::new();

        for (tx_hash, details) in &swaps_in_block {
            let pools_in_tx: HashSet<&str> =
                details.iter().map(|d| d.pool_address.as_str()).collect();

            let has_a = pools_in_tx.contains(fixture_pool_a.as_str());
            let has_b = pools_in_tx.contains(fixture_pool_b.as_str());

            if has_a && has_b {
                cross_dex_txs.push((tx_hash.clone(), details.clone()));
            } else if has_a || has_b {
                any_pool_txs.push((tx_hash.clone(), details.clone()));
            }
        }

        let (status, arb_tx, detail_strings) = if !cross_dex_txs.is_empty() {
            confirmed_count += 1;
            let best_tx = &cross_dex_txs[0];
            let details: Vec<String> = best_tx.1.iter().map(|d| d.to_string()).collect();
            ("CONFIRMED_CROSS_DEX_ARB", Some(best_tx.0.clone()), details)
        } else if !any_pool_txs.is_empty() {
            // Swaps on individual pools exist but not paired in one tx —
            // the arb opportunity existed but may have been captured differently
            opportunity_only_count += 1;
            let pool_names: Vec<String> = any_pool_txs
                .iter()
                .flat_map(|(_, ds)| ds.iter().map(|d| d.pool_name.clone()))
                .collect();
            (
                "OPPORTUNITY_CONFIRMED",
                None,
                vec![format!("single-pool swaps: {}", pool_names.join(", "))],
            )
        } else {
            // No Swap events on tracked pools at all in this block.
            // The fixture detected an arb from pre-block reserves —
            // either it was captured via a different contract (not V2 Swap)
            // or nobody took it.
            opportunity_only_count += 1;
            (
                "OPPORTUNITY_ONLY",
                None,
                vec!["no tracked-pool Swap events in block".to_string()],
            )
        };

        let verification_url = match &arb_tx {
            Some(h) => format!("https://etherscan.io/tx/{}#eventlog", h),
            None => format!("https://etherscan.io/txs?block={}", fixture.block_number),
        };

        let short_pair = if fixture.pair.len() > 30 {
            format!("{}...", &fixture.pair[..30])
        } else {
            fixture.pair.clone()
        };

        let arb_tx_short = arb_tx
            .as_deref()
            .map(|h| format!("{}...", &h[..14]))
            .unwrap_or_else(|| "-".to_string());

        eprintln!(
            "{:<10} | {:<32} | {:<28} | {:<8} | {:<50} | {}",
            fixture.block_number,
            short_pair,
            status,
            cross_dex_txs.len(),
            arb_tx_short,
            detail_strings.first().unwrap_or(&String::new()),
        );
        for detail in detail_strings.iter().skip(1) {
            eprintln!(
                "{:<10} | {:<32} | {:<28} | {:<8} | {:<50} | {}",
                "", "", "", "", "", detail,
            );
        }

        // Enrich fixture entry
        fixture.verification_url = Some(verification_url);
        fixture.verification_method = Some("etherscan_getLogs_block_swap_scan".to_string());
        fixture.verification_status = Some(status.to_string());
        fixture.actual_arb_tx_hash = arb_tx;
        fixture.actual_arb_tx_count = Some(cross_dex_txs.len());
        fixture.swap_details = Some(detail_strings);
    }

    // ---- Phase 3: Profit verification via tx receipts ----
    // For each CONFIRMED fixture, fetch the receipt, decode Transfer events,
    // build balance deltas, and extract the arbitrageur's WETH P&L.
    let confirmed_fixtures: Vec<usize> = fixtures
        .iter()
        .enumerate()
        .filter(|(_, f)| f.verification_status.as_deref() == Some("CONFIRMED_CROSS_DEX_ARB"))
        .map(|(i, _)| i)
        .collect();

    eprintln!(
        "\n=== PROFIT VERIFICATION ({} confirmed fixtures) ===\n",
        confirmed_fixtures.len()
    );
    eprintln!(
        "{:<12} | {:<8} | {:<22} | {:<22} | {:<22} | profit_check",
        "tx_hash", "block", "gross_profit_wei", "gas_cost_wei", "net_profit_wei"
    );
    eprintln!("{}", "-".repeat(120));

    let mut profit_match_count: usize = 0;
    let mut profit_mismatch_count: usize = 0;
    let mut profit_error_count: usize = 0;

    // deduplicate tx_hash — block 15538813 has two fixture entries for the same tx
    let mut seen_tx_hashes: HashSet<String> = HashSet::new();
    // Cache receipts by tx_hash so we don't fetch the same receipt twice
    let mut receipt_cache: HashMap<String, serde_json::Value> = HashMap::new();

    for &idx in &confirmed_fixtures {
        let fixture = &fixtures[idx];
        let arb_tx_hash = fixture
            .actual_arb_tx_hash
            .as_deref()
            .unwrap_or(&fixture.tx_hash)
            .to_lowercase();

        // Rate limit (free tier: 5 calls/sec)
        tokio::time::sleep(Duration::from_millis(220)).await;

        // Fetch receipt if not cached
        if !receipt_cache.contains_key(&arb_tx_hash) {
            match fetch_receipt(&client, &api_key, &arb_tx_hash).await {
                Ok(r) => {
                    receipt_cache.insert(arb_tx_hash.clone(), r);
                }
                Err(e) => {
                    eprintln!(
                        "{:<12} | {:<8} | ERROR: {}",
                        &arb_tx_hash[..12],
                        fixture.block_number,
                        e
                    );
                    profit_error_count += 1;
                    continue;
                }
            }
        }

        let receipt = &receipt_cache[&arb_tx_hash];

        // 1. Extract all logs from the receipt
        let logs = receipt
            .get("logs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // 2. Build balance delta map from Transfer events
        let deltas = build_balance_deltas(&logs);

        // 3. Identify the tx sender (EOA) and the arb contract (receipt.to).
        //    MEV arbs typically go through a contract — profit lands in the
        //    contract, not the EOA.  We check BOTH and pick the one with
        //    the higher WETH balance delta.
        let tx_sender = receipt
            .get("from")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let arb_contract = receipt
            .get("to")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let sender_weth_delta: i128 = deltas
            .get(&tx_sender)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0);

        let contract_weth_delta: i128 = deltas
            .get(&arb_contract)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0);

        // 4. Gross profit = best WETH delta between EOA and contract
        let (gross_profit_wei, profiteer) = if contract_weth_delta > sender_weth_delta {
            (
                contract_weth_delta,
                format!("contract({}…)", &arb_contract[..10]),
            )
        } else {
            (sender_weth_delta, format!("sender({}…)", &tx_sender[..10]))
        };

        // 5. Gas cost = gasUsed * effectiveGasPrice
        let gas_used = receipt
            .get("gasUsed")
            .and_then(|v| v.as_str())
            .and_then(parse_hex_u128)
            .unwrap_or(0);
        let effective_gas_price = receipt
            .get("effectiveGasPrice")
            .and_then(|v| v.as_str())
            .and_then(parse_hex_u128)
            .unwrap_or(0);
        let gas_cost_wei: u128 = gas_used.saturating_mul(effective_gas_price);

        // 6. Net profit = gross - gas (i128 to handle negatives)
        let net_profit_wei: i128 = gross_profit_wei - gas_cost_wei as i128;

        // 7. Compare against fixture's expected profit
        let expected = fixture.profit_approx_wei as i128;
        let profit_check = if expected == 0 && gross_profit_wei == 0 {
            profit_match_count += 1;
            "MATCH (both zero)".to_string()
        } else if expected == 0 {
            // Expected was a placeholder — just report the verified value
            profit_match_count += 1;
            format!("NEW (gross={})", gross_profit_wei)
        } else {
            let deviation_pct = if expected != 0 {
                ((gross_profit_wei - expected) as f64 / expected as f64 * 100.0).abs()
            } else {
                100.0
            };
            if deviation_pct <= 10.0 {
                profit_match_count += 1;
                format!("MATCH ({:.1}% dev)", deviation_pct)
            } else {
                profit_mismatch_count += 1;
                format!(
                    "WARNING: {:.1}% deviation (expected={}, verified={})",
                    deviation_pct, expected, gross_profit_wei
                )
            }
        };

        let short_hash = format!("{}…", &arb_tx_hash[..10]);
        eprintln!(
            "{:<12} | {:<8} | {:<22} | {:<22} | {:<22} | {}",
            short_hash,
            fixture.block_number,
            gross_profit_wei,
            gas_cost_wei,
            net_profit_wei,
            profit_check,
        );
        eprintln!("             profiteer: {}", profiteer);

        // Log non-WETH token deltas for the sender and contract (diagnostics)
        for addr_label in &[(&tx_sender, "sender"), (&arb_contract, "contract")] {
            if let Some(token_map) = deltas.get(addr_label.0) {
                for (token, delta) in token_map {
                    if token != WETH_ADDRESS && *delta != 0 {
                        eprintln!(
                            "             {}_DELTA: {}…  {}",
                            addr_label.1,
                            &token[..10],
                            delta,
                        );
                    }
                }
            }
        }

        // Skip duplicate enrichment for same tx_hash (block 15538813 two entries)
        if seen_tx_hashes.contains(&arb_tx_hash) {
            // Still enrich the fixture entry
            let f = &mut fixtures[idx];
            f.verified_gross_profit_wei = Some(gross_profit_wei.to_string());
            f.verified_net_profit_wei = Some(net_profit_wei.to_string());
            f.verified_gas_cost_wei = Some(gas_cost_wei.to_string());
            continue;
        }
        seen_tx_hashes.insert(arb_tx_hash.clone());

        // Enrich fixture
        let f = &mut fixtures[idx];
        f.verified_gross_profit_wei = Some(gross_profit_wei.to_string());
        f.verified_net_profit_wei = Some(net_profit_wei.to_string());
        f.verified_gas_cost_wei = Some(gas_cost_wei.to_string());
    }

    // ---- Write enriched JSON back ----
    let enriched_json =
        serde_json::to_string_pretty(&fixtures).expect("fixture serialization should succeed");
    std::fs::write(&path, format!("{}\n", enriched_json))
        .expect("writing enriched known_arb_txs.json should succeed");
    eprintln!(
        "\nWrote enriched fixtures to {}",
        path.canonicalize().unwrap_or(path.clone()).display()
    );

    // ---- Final summary ----
    eprintln!("\n=== VERIFICATION SUMMARY ===");
    eprintln!("  Total fixtures:          {}", fixtures.len());
    eprintln!("  CONFIRMED_CROSS_DEX_ARB: {}", confirmed_count);
    eprintln!("  OPPORTUNITY_CONFIRMED/ONLY: {}", opportunity_only_count);
    eprintln!();
    eprintln!("=== PROFIT VERIFICATION ===");
    eprintln!("  Profit verified (match): {}", profit_match_count);
    eprintln!("  Profit deviation (>10%): {}", profit_mismatch_count);
    eprintln!("  Profit fetch errors:     {}", profit_error_count);

    // Soft assertion: all fixtures got a verification status
    assert!(
        fixtures.iter().all(|f| f.verification_status.is_some()),
        "All fixtures should have a verification_status after scan",
    );

    // Assert all confirmed fixtures got profit data
    let profit_verified = fixtures
        .iter()
        .filter(|f| {
            f.verification_status.as_deref() == Some("CONFIRMED_CROSS_DEX_ARB")
                && f.verified_gross_profit_wei.is_some()
        })
        .count();
    eprintln!(
        "  Profit data present:     {}/{} confirmed",
        profit_verified, confirmed_count
    );
    assert_eq!(
        profit_verified, confirmed_count,
        "All confirmed fixtures should have verified_gross_profit_wei",
    );
}

// ---------- offline unit tests ----------

#[test]
fn decode_swap_data_parses_valid_256byte_payload() {
    // a0in=1000, a1in=0, a0out=0, a1out=2000
    let mut hex = String::from("0x");
    hex.push_str(&format!("{:064x}", 1000u128));
    hex.push_str(&format!("{:064x}", 0u128));
    hex.push_str(&format!("{:064x}", 0u128));
    hex.push_str(&format!("{:064x}", 2000u128));

    let (a0in, a1in, a0out, a1out) = decode_swap_data(&hex).expect("should decode");
    assert_eq!(a0in, 1000);
    assert_eq!(a1in, 0);
    assert_eq!(a0out, 0);
    assert_eq!(a1out, 2000);
}

#[test]
fn decode_swap_data_rejects_short_payload() {
    assert!(decode_swap_data("0xdead").is_none());
    assert!(decode_swap_data("").is_none());
}

#[test]
fn tracked_pool_map_has_six_entries() {
    let map = tracked_pool_map();
    assert_eq!(map.len(), 6, "should track exactly 6 pools");
    assert!(map.contains_key("0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc"));
    assert!(map.contains_key("0x397ff1542f962076d0bfe58ea045ffa2d347aca0"));
}

#[test]
fn fixture_path_points_to_test_data_dir() {
    let p = fixture_path();
    assert!(
        p.ends_with("test_data/known_arb_txs.json"),
        "expected path ending with test_data/known_arb_txs.json, got: {}",
        p.display()
    );
}

#[test]
fn decode_transfer_amount_parses_one_ether() {
    let data = format!("0x{:064x}", 1_000_000_000_000_000_000u128);
    let amount = decode_transfer_amount(&data).expect("should decode");
    assert_eq!(amount, 1_000_000_000_000_000_000u128);
}

#[test]
fn decode_transfer_amount_handles_zero() {
    let data = format!("0x{:064x}", 0u128);
    let amount = decode_transfer_amount(&data).expect("should decode zero");
    assert_eq!(amount, 0);
}

#[test]
fn address_from_topic_extracts_correctly() {
    let topic = "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
    let addr = address_from_topic(topic).expect("should extract");
    assert_eq!(addr, "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
}

#[test]
fn address_from_topic_rejects_short_input() {
    assert!(address_from_topic("0xdead").is_none());
}

#[test]
fn parse_hex_u128_handles_common_patterns() {
    assert_eq!(parse_hex_u128("0x0"), Some(0));
    assert_eq!(parse_hex_u128("0x1"), Some(1));
    assert_eq!(
        parse_hex_u128("0xde0b6b3a7640000"),
        Some(1_000_000_000_000_000_000)
    );
    // Leading zeros
    assert_eq!(
        parse_hex_u128("0x00000000000000000000000000000001"),
        Some(1)
    );
}

#[test]
fn build_balance_deltas_tracks_bidirectional_transfers() {
    // Simulate: alice sends 100 WETH to bob
    let alice_topic = "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let bob_topic = "0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let amount_hex = format!("0x{:064x}", 100u128);

    let logs = vec![serde_json::json!({
        "address": WETH_ADDRESS,
        "topics": [TRANSFER_TOPIC, alice_topic, bob_topic],
        "data": amount_hex,
    })];

    let deltas = build_balance_deltas(&logs);
    let alice = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let bob = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    assert_eq!(
        deltas
            .get(alice)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0),
        -100
    );
    assert_eq!(
        deltas
            .get(bob)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0),
        100
    );
}

#[test]
fn build_balance_deltas_aggregates_multiple_transfers() {
    let alice = "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let bob = "0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let carol = "0x000000000000000000000000cccccccccccccccccccccccccccccccccccccccc";

    let logs = vec![
        // bob -> alice: 200 WETH
        serde_json::json!({
            "address": WETH_ADDRESS,
            "topics": [TRANSFER_TOPIC, bob, alice],
            "data": format!("0x{:064x}", 200u128),
        }),
        // alice -> carol: 50 WETH
        serde_json::json!({
            "address": WETH_ADDRESS,
            "topics": [TRANSFER_TOPIC, alice, carol],
            "data": format!("0x{:064x}", 50u128),
        }),
    ];

    let deltas = build_balance_deltas(&logs);
    let alice_addr = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    // alice: +200 - 50 = +150
    assert_eq!(
        deltas
            .get(alice_addr)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0),
        150
    );
}

#[test]
fn build_balance_deltas_tracks_multiple_tokens() {
    let alice = "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let bob = "0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let usdc = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";

    let logs = vec![
        // WETH: bob -> alice 1 ETH
        serde_json::json!({
            "address": WETH_ADDRESS,
            "topics": [TRANSFER_TOPIC, bob, alice],
            "data": format!("0x{:064x}", 1_000_000_000_000_000_000u128),
        }),
        // USDC: alice -> bob 1500 USDC (6 decimals)
        serde_json::json!({
            "address": usdc,
            "topics": [TRANSFER_TOPIC, alice, bob],
            "data": format!("0x{:064x}", 1_500_000_000u128),
        }),
    ];

    let deltas = build_balance_deltas(&logs);
    let alice_addr = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    assert_eq!(
        deltas
            .get(alice_addr)
            .and_then(|m| m.get(WETH_ADDRESS))
            .copied()
            .unwrap_or(0),
        1_000_000_000_000_000_000i128
    );
    assert_eq!(
        deltas
            .get(alice_addr)
            .and_then(|m| m.get(usdc))
            .copied()
            .unwrap_or(0),
        -1_500_000_000i128
    );
}
