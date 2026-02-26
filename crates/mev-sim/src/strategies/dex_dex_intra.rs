//! Intra-block DEX-DEX scanner using Uniswap V2 `Sync` logs.
//!
//! Reconstructs reserve snapshots after each in-block state mutation and evaluates
//! cross-DEX opportunities without full transaction replay.
//!
//! ## Merged Timeline Algorithm
//!
//! All `Sync` logs from every tracked pool are merged into one timeline sorted by
//! `(tx_index, log_index)`. A `HashMap<Address, (u128, u128)>` tracks the latest
//! reserves for every pool. After each snapshot we re-evaluate every pair that
//! references the updated pool, using `detect_v2_arb_opportunity_with_reason` for
//! reject-reason instrumentation.
//!
//! ## Instrumentation
//!
//! - `MEV_INTRA_DEBUG=1`: per-block summary (sync counts, max spread, candidate count)
//! - `MEV_INTRA_DUMP_BLOCK=<N>`: dumps first ~30 timeline steps with human-readable
//!   prices, reserve states, and verdict reasons for block `N`
//!
//! ## Known Limitations
//!
//! - **V2 `Sync` events only:** Tracks reserve state changes via the UniV2 `Sync`
//!   log topic. Uniswap V3 `Swap` events (different signature and price model)
//!   are not tracked, making the scanner blind to V3 intra-block price movements.
//! - **Same 6-pool default set:** Uses [`DEFAULT_ARB_PAIRS`] plus 3 stable-stable
//!   pairs = 12 pools. At block 17M, these pools see 0–1 Sync events per block
//!   while ~17 V2 Swap events occur on unscanned long-tail pools.
//! - **60 bps fee floor still applies:** Even within a block, two-hop V2 arbs
//!   must overcome the 60 bps round-trip fee, which is rarely breached except
//!   during extreme volatility (e.g., the USDC depeg at block ~16.8M).

use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use eyre::{eyre, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::decoder::addresses;

use super::arbitrage::{
    detect_v2_arb_opportunity_with_reason, spread_bps_integer, ArbPairConfig, PoolState,
    ReserveReader, DEFAULT_ARB_PAIRS,
};

const SYNC_TOPIC0: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";

/// Minimum spread (integer bps) that triggers a candidate row even when sizing fails.
const CANDIDATE_TRIGGER_BPS: u128 = 30;

/// Maximum timeline steps dumped by `MEV_INTRA_DUMP_BLOCK`.
const DUMP_STEP_LIMIT: usize = 30;

fn intra_debug_enabled() -> bool {
    std::env::var("MEV_INTRA_DEBUG")
        .ok()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn intra_dump_block() -> Option<u64> {
    std::env::var("MEV_INTRA_DUMP_BLOCK")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
}

/// Intra-block opportunity emitted after a specific transaction/log boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntraBlockArb {
    /// Block number being scanned.
    pub block_number: u64,
    /// Transaction index after which this opportunity is visible.
    pub after_tx_index: u64,
    /// Log index after which this opportunity is visible.
    pub after_log_index: u64,
    /// First pool in route.
    pub pool_a: Address,
    /// Second pool in route.
    pub pool_b: Address,
    /// Spread in integer basis points (cross-multiplication, no float).
    pub spread_bps: u128,
    /// Expected profit in wei (0 for candidate-trigger-only rows).
    pub profit_wei: u128,
    /// Route direction label.
    pub direction: String,
    /// Verdict reason from the arbitrage detector (empty for successful opportunities).
    pub verdict: String,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLog {
    address: String,
    data: String,
    topics: Vec<String>,
    transaction_index: Option<String>,
    log_index: Option<String>,
}

#[derive(Clone, Debug)]
struct ReserveSnapshot {
    pool: Address,
    tx_index: u64,
    log_index: u64,
    reserve0: u128,
    reserve1: u128,
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    let trimmed = value.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(trimmed, 16)
        .map_err(|error| eyre!("failed to parse hex u64 '{}': {}", value, error))
}

fn parse_hex_u128(value: &str) -> Result<u128> {
    let trimmed = value.trim_start_matches("0x");
    if trimmed.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(trimmed, 16)
        .map_err(|error| eyre!("failed to parse hex u128 '{}': {}", value, error))
}

fn decode_sync_data(data: &str) -> Result<(u128, u128)> {
    let trimmed = data.trim_start_matches("0x");
    if trimmed.len() < 128 {
        return Err(eyre!("sync data payload too short: {}", data));
    }

    let reserve0 = parse_hex_u128(&trimmed[0..64])?;
    let reserve1 = parse_hex_u128(&trimmed[64..128])?;
    Ok((reserve0, reserve1))
}

/// Normalize reserves so token_a maps to reserve0 (sorted by address).
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

/// Fetches `Sync` event logs from `eth_getLogs` for the specified pools in a single block.
///
/// # Errors
/// Returns error when the RPC request fails or returns an error response.
async fn fetch_sync_logs(
    rpc_url: &str,
    block_number: u64,
    pools: &[Address],
) -> Result<Vec<RawLog>> {
    if pools.is_empty() {
        return Ok(Vec::new());
    }

    let addresses: Vec<String> = pools
        .iter()
        .map(|address| format!("{address:#x}"))
        .collect();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getLogs",
        "params": [{
            "fromBlock": format!("0x{:x}", block_number),
            "toBlock": format!("0x{:x}", block_number),
            "address": addresses,
            "topics": [SYNC_TOPIC0],
        }]
    });

    let response = Client::new()
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .map_err(|error| eyre!("eth_getLogs request failed: {}", error))?;

    let status = response.status();
    let body: RpcResponse<Vec<RawLog>> = response
        .json()
        .await
        .map_err(|error| eyre!("failed decoding eth_getLogs response: {}", error))?;

    if !status.is_success() {
        return Err(eyre!("eth_getLogs returned HTTP status {}", status));
    }

    if let Some(error) = body.error {
        return Err(eyre!(
            "eth_getLogs RPC error {}: {}",
            error.code,
            error.message
        ));
    }

    Ok(body.result.unwrap_or_default())
}

fn decode_snapshots(logs: &[RawLog]) -> Result<Vec<ReserveSnapshot>> {
    let mut snapshots = Vec::new();

    for log in logs {
        if log.topics.first().map(|value| value.to_lowercase()) != Some(SYNC_TOPIC0.to_string()) {
            continue;
        }

        let address = log
            .address
            .parse::<Address>()
            .map_err(|error| eyre!("failed to parse log address {}: {}", log.address, error))?;
        let tx_index_hex = log
            .transaction_index
            .as_deref()
            .ok_or_else(|| eyre!("missing transactionIndex in sync log"))?;
        let log_index_hex = log
            .log_index
            .as_deref()
            .ok_or_else(|| eyre!("missing logIndex in sync log"))?;
        let (reserve0, reserve1) = decode_sync_data(&log.data)?;

        snapshots.push(ReserveSnapshot {
            pool: address,
            tx_index: parse_hex_u64(tx_index_hex)?,
            log_index: parse_hex_u64(log_index_hex)?,
            reserve0,
            reserve1,
        });
    }

    snapshots.sort_by_key(|entry| (entry.tx_index, entry.log_index));
    Ok(snapshots)
}

/// Builds a `PoolState` from the latest reserves for a given pool address.
fn pool_state_from_latest(
    pool_addr: Address,
    config: &ArbPairConfig,
    reserves: (u128, u128),
    block_number: u64,
    timestamp_last: u64,
) -> PoolState {
    let (token_a_reserve, token_b_reserve) =
        normalized_reserves(config.token_a, config.token_b, reserves.0, reserves.1);
    PoolState {
        address: pool_addr,
        token0: config.token_a,
        token1: config.token_b,
        reserve0: token_a_reserve,
        reserve1: token_b_reserve,
        fee_numerator: 997,
        fee_denominator: 1000,
        block_number,
        timestamp_last,
    }
}

/// Scans in-block `Sync` transitions for cross-DEX arbitrage opportunities.
///
/// Uses a merged timeline of all `Sync` logs across both DEXes. After each
/// snapshot, every pair referencing the updated pool is re-evaluated via
/// `detect_v2_arb_opportunity_with_reason`.
///
/// Emits candidate-trigger rows when spread >= `CANDIDATE_TRIGGER_BPS` even if
/// the sizing solver rejects the opportunity (profit=0, verdict populated).
///
/// # Errors
/// Returns error when log or reserve RPC reads fail.
pub async fn scan_dex_dex_intra_block(
    rpc_url: &str,
    block_number: u64,
    base_fee: u128,
    pair_configs: &[ArbPairConfig],
) -> Result<Vec<IntraBlockArb>> {
    let pre_state_block = block_number.saturating_sub(1);
    let reserve_reader = ReserveReader::new(rpc_url, pre_state_block);

    // Resolve pair addresses and collect tracked pools.
    let mut resolved_pairs: Vec<(ArbPairConfig, Address, Address)> = Vec::new();
    let mut tracked_pools = HashSet::new();
    // Map pool address -> set of pair indices that reference it.
    let mut pool_to_pairs: HashMap<Address, Vec<usize>> = HashMap::new();

    for config in pair_configs {
        let pool_a = reserve_reader
            .get_pair(config.dex_a_factory, config.token_a, config.token_b)
            .await?;
        let pool_b = reserve_reader
            .get_pair(config.dex_b_factory, config.token_a, config.token_b)
            .await?;

        if let (Some(pool_a), Some(pool_b)) = (pool_a, pool_b) {
            let pair_idx = resolved_pairs.len();
            tracked_pools.insert(pool_a);
            tracked_pools.insert(pool_b);
            pool_to_pairs.entry(pool_a).or_default().push(pair_idx);
            pool_to_pairs.entry(pool_b).or_default().push(pair_idx);
            resolved_pairs.push((*config, pool_a, pool_b));
        }
    }

    if resolved_pairs.is_empty() {
        return Ok(Vec::new());
    }

    let tracked_pool_list: Vec<Address> = tracked_pools.iter().copied().collect();

    // Log pool metadata once at scan start.
    if intra_debug_enabled() {
        for (idx, (config, pool_a, pool_b)) in resolved_pairs.iter().enumerate() {
            tracing::debug!(
                pair_idx = idx,
                token_a = %format!("{:#x}", config.token_a),
                token_b = %format!("{:#x}", config.token_b),
                pool_a = %format!("{:#x}", pool_a),
                pool_b = %format!("{:#x}", pool_b),
                "intra-block pair resolved"
            );
        }
    }

    // Fetch all Sync logs for this block.
    let logs = fetch_sync_logs(rpc_url, block_number, &tracked_pool_list).await?;
    let timeline = decode_snapshots(&logs)?;

    let dump_this_block = intra_dump_block() == Some(block_number);
    let debug = intra_debug_enabled();

    // Count per-pool sync events for instrumentation.
    let mut sync_counts: HashMap<Address, usize> = HashMap::new();
    for snapshot in &timeline {
        *sync_counts.entry(snapshot.pool).or_default() += 1;
    }

    if debug {
        tracing::debug!(
            block_number,
            tracked_pools = tracked_pool_list.len(),
            sync_logs = logs.len(),
            snapshots = timeline.len(),
            "intra-block timeline loaded"
        );
        for (pool, count) in &sync_counts {
            tracing::debug!(
                pool = %format!("{:#x}", pool),
                sync_count = count,
                "pool sync activity"
            );
        }
    }

    // Initialize latest reserves from pre-state (block N-1).
    let mut latest: HashMap<Address, (u128, u128, u64)> = HashMap::new();
    for pool in &tracked_pool_list {
        let (r0, r1, ts) = reserve_reader.get_reserves(*pool).await?;
        latest.insert(*pool, (r0, r1, ts));
    }

    let mut opportunities = Vec::new();
    let mut max_spread_bps: u128 = 0;
    let mut dump_step: usize = 0;

    // Walk the merged timeline.
    for snapshot in &timeline {
        let timestamp_last = latest
            .get(&snapshot.pool)
            .map(|value| value.2)
            .unwrap_or_default();
        latest.insert(
            snapshot.pool,
            (snapshot.reserve0, snapshot.reserve1, timestamp_last),
        );

        // Only evaluate pairs that reference the pool that just changed.
        let pair_indices = match pool_to_pairs.get(&snapshot.pool) {
            Some(indices) => indices.clone(),
            None => continue,
        };

        for &pair_idx in &pair_indices {
            let (config, pool_a_addr, pool_b_addr) = &resolved_pairs[pair_idx];

            let Some((a_r0, a_r1, a_ts)) = latest.get(pool_a_addr).copied() else {
                continue;
            };
            let Some((b_r0, b_r1, b_ts)) = latest.get(pool_b_addr).copied() else {
                continue;
            };

            let pool_a_state =
                pool_state_from_latest(*pool_a_addr, config, (a_r0, a_r1), block_number, a_ts);
            let pool_b_state =
                pool_state_from_latest(*pool_b_addr, config, (b_r0, b_r1), block_number, b_ts);

            let spread = spread_bps_integer(&pool_a_state, &pool_b_state);
            if spread > max_spread_bps {
                max_spread_bps = spread;
            }

            // Select reference pool and base_fee for gas conversion.
            let reference_pool =
                if config.token_a == addresses::WETH || config.token_b == addresses::WETH {
                    Some(&pool_a_state)
                } else {
                    None
                };

            let detection_base_fee = if reference_pool.is_some() {
                base_fee
            } else {
                0 // stable-stable pairs: no gas deduction in token terms
            };

            let arb_result = detect_v2_arb_opportunity_with_reason(
                &pool_a_state,
                &pool_b_state,
                detection_base_fee,
                reference_pool,
            );

            let (maybe_opportunity, verdict_str) = match arb_result {
                Ok((opp, verdict)) => (opp, verdict),
                Err(arb_error) => {
                    if debug {
                        tracing::warn!(
                            block_number,
                            tx_index = snapshot.tx_index,
                            log_index = snapshot.log_index,
                            error = %arb_error,
                            "arb detection error"
                        );
                    }
                    continue;
                }
            };

            // Dump mode: emit first N timeline steps.
            if dump_this_block && dump_step < DUMP_STEP_LIMIT {
                dump_step += 1;
                tracing::info!(
                    step = dump_step,
                    block_number,
                    tx_index = snapshot.tx_index,
                    log_index = snapshot.log_index,
                    changed_pool = %format!("{:#x}", snapshot.pool),
                    pool_a = %format!("{:#x}", pool_a_state.address),
                    pool_a_r0 = pool_a_state.reserve0,
                    pool_a_r1 = pool_a_state.reserve1,
                    pool_b = %format!("{:#x}", pool_b_state.address),
                    pool_b_r0 = pool_b_state.reserve0,
                    pool_b_r1 = pool_b_state.reserve1,
                    spread_bps = spread,
                    verdict = %verdict_str,
                    "INTRA_DUMP"
                );
            }

            if let Some(opportunity) = maybe_opportunity {
                opportunities.push(IntraBlockArb {
                    block_number,
                    after_tx_index: snapshot.tx_index,
                    after_log_index: snapshot.log_index,
                    pool_a: opportunity.pool_1,
                    pool_b: opportunity.pool_2,
                    spread_bps: spread,
                    profit_wei: opportunity.net_profit_wei,
                    direction: format!("{:#x}->{:#x}", opportunity.pool_1, opportunity.pool_2),
                    verdict: String::new(),
                });
            } else if spread >= CANDIDATE_TRIGGER_BPS {
                // Candidate-trigger row: spread is significant but sizing/profit failed.
                opportunities.push(IntraBlockArb {
                    block_number,
                    after_tx_index: snapshot.tx_index,
                    after_log_index: snapshot.log_index,
                    pool_a: pool_a_state.address,
                    pool_b: pool_b_state.address,
                    spread_bps: spread,
                    profit_wei: 0,
                    direction: format!(
                        "{:#x}->{:#x}:candidate",
                        pool_a_state.address, pool_b_state.address
                    ),
                    verdict: verdict_str.clone(),
                });

                if debug {
                    tracing::debug!(
                        block_number,
                        tx_index = snapshot.tx_index,
                        log_index = snapshot.log_index,
                        pool_a = %format!("{:#x}", pool_a_state.address),
                        pool_b = %format!("{:#x}", pool_b_state.address),
                        spread_bps = spread,
                        verdict = %verdict_str,
                        "candidate trigger (spread above threshold, no profit)"
                    );
                }
            }
        }
    }

    // Per-block summary when debug enabled.
    if debug {
        tracing::debug!(
            block_number,
            total_syncs = timeline.len(),
            max_spread_bps,
            profitable_count = opportunities.iter().filter(|o| o.profit_wei > 0).count(),
            candidate_count = opportunities.iter().filter(|o| o.profit_wei == 0).count(),
            "intra-block scan complete"
        );
    }

    // Sort: profitable first (descending), then candidates.
    opportunities.sort_by(|left, right| right.profit_wei.cmp(&left.profit_wei));
    Ok(opportunities)
}

/// Scans default pair universe using intra-block `Sync` logs.
///
/// Includes WETH-stablecoin pairs plus stable-stable pairs for comprehensive coverage.
///
/// # Errors
/// Returns error when reserve or log RPC calls fail.
pub async fn scan_default_dex_dex_intra_block(
    rpc_url: &str,
    block_number: u64,
    base_fee: u128,
) -> Result<Vec<IntraBlockArb>> {
    let mut pair_configs = DEFAULT_ARB_PAIRS.to_vec();
    let dex_a_factory = DEFAULT_ARB_PAIRS[0].dex_a_factory;
    let dex_b_factory = DEFAULT_ARB_PAIRS[0].dex_b_factory;

    pair_configs.extend([
        ArbPairConfig {
            token_a: addresses::USDC,
            token_b: addresses::USDT,
            dex_a_factory,
            dex_b_factory,
        },
        ArbPairConfig {
            token_a: addresses::USDC,
            token_b: addresses::DAI,
            dex_a_factory,
            dex_b_factory,
        },
        ArbPairConfig {
            token_a: addresses::USDT,
            token_b: addresses::DAI,
            dex_a_factory,
            dex_b_factory,
        },
    ]);

    scan_dex_dex_intra_block(rpc_url, block_number, base_fee, &pair_configs).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_sync_data_valid() {
        let r0 = 1_000_000u128;
        let r1 = 2_000_000u128;
        let data = format!("0x{:064x}{:064x}", r0, r1);
        let (parsed_r0, parsed_r1) = decode_sync_data(&data).expect("should decode");
        assert_eq!(parsed_r0, r0);
        assert_eq!(parsed_r1, r1);
    }

    #[test]
    fn decode_sync_data_too_short() {
        assert!(decode_sync_data("0xaabb").is_err());
    }

    #[test]
    fn decode_snapshots_sorts_by_tx_and_log_index() {
        let pool: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let logs = vec![
            RawLog {
                address: format!("{pool:#x}"),
                data: format!("0x{:064x}{:064x}", 100u128, 200u128),
                topics: vec![SYNC_TOPIC0.to_string()],
                transaction_index: Some("0x5".to_string()),
                log_index: Some("0x2".to_string()),
            },
            RawLog {
                address: format!("{pool:#x}"),
                data: format!("0x{:064x}{:064x}", 300u128, 400u128),
                topics: vec![SYNC_TOPIC0.to_string()],
                transaction_index: Some("0x2".to_string()),
                log_index: Some("0x0".to_string()),
            },
        ];

        let snapshots = decode_snapshots(&logs).expect("should decode");
        assert_eq!(snapshots.len(), 2);
        // Second log should come first because tx_index 2 < 5.
        assert_eq!(snapshots[0].tx_index, 2);
        assert_eq!(snapshots[0].reserve0, 300);
        assert_eq!(snapshots[1].tx_index, 5);
        assert_eq!(snapshots[1].reserve0, 100);
    }

    #[test]
    fn normalized_reserves_respects_address_order() {
        let low: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let high: Address = "0x0000000000000000000000000000000000000002"
            .parse()
            .unwrap();

        // When token_a < token_b, reserves stay as-is.
        assert_eq!(normalized_reserves(low, high, 100, 200), (100, 200));
        // When token_a > token_b, reserves swap.
        assert_eq!(normalized_reserves(high, low, 100, 200), (200, 100));
    }

    #[test]
    fn intra_block_arb_has_integer_spread() {
        let arb = IntraBlockArb {
            block_number: 18_000_000,
            after_tx_index: 5,
            after_log_index: 12,
            pool_a: Address::ZERO,
            pool_b: Address::ZERO,
            spread_bps: 150,
            profit_wei: 1_000_000,
            direction: "test".to_string(),
            verdict: String::new(),
        };
        assert_eq!(arb.spread_bps, 150u128);
    }

    #[test]
    fn pool_state_from_latest_normalizes_correctly() {
        let pool_addr: Address = "0x0000000000000000000000000000000000000099"
            .parse()
            .unwrap();
        let token_a: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();
        let token_b: Address = "0x0000000000000000000000000000000000000002"
            .parse()
            .unwrap();

        let config = ArbPairConfig {
            token_a,
            token_b,
            dex_a_factory: Address::ZERO,
            dex_b_factory: Address::ZERO,
        };

        let state = pool_state_from_latest(pool_addr, &config, (1000, 2000), 18_000_000, 12345);
        assert_eq!(state.address, pool_addr);
        assert_eq!(state.token0, token_a);
        assert_eq!(state.token1, token_b);
        assert_eq!(state.reserve0, 1000);
        assert_eq!(state.reserve1, 2000);
        assert_eq!(state.block_number, 18_000_000);
        assert_eq!(state.timestamp_last, 12345);
    }

    #[test]
    fn snapshot_timeline_merge_walks_in_order() {
        let pool_uni: Address = "0x0000000000000000000000000000000000000010"
            .parse()
            .unwrap();
        let pool_sushi: Address = "0x0000000000000000000000000000000000000020"
            .parse()
            .unwrap();

        let mut snapshots = [
            ReserveSnapshot {
                pool: pool_uni,
                tx_index: 0,
                log_index: 3,
                reserve0: 1000,
                reserve1: 2000,
            },
            ReserveSnapshot {
                pool: pool_sushi,
                tx_index: 1,
                log_index: 0,
                reserve0: 1100,
                reserve1: 2200,
            },
            ReserveSnapshot {
                pool: pool_uni,
                tx_index: 1,
                log_index: 5,
                reserve0: 1050,
                reserve1: 2100,
            },
            ReserveSnapshot {
                pool: pool_sushi,
                tx_index: 3,
                log_index: 1,
                reserve0: 1200,
                reserve1: 2400,
            },
        ];
        snapshots.sort_by_key(|s| (s.tx_index, s.log_index));

        assert_eq!(snapshots[0].pool, pool_uni);
        assert_eq!(snapshots[0].tx_index, 0);
        assert_eq!(snapshots[1].pool, pool_sushi);
        assert_eq!(snapshots[1].tx_index, 1);
        assert_eq!(snapshots[2].pool, pool_uni);
        assert_eq!((snapshots[2].tx_index, snapshots[2].log_index), (1, 5));
        assert_eq!(snapshots[3].pool, pool_sushi);
        assert_eq!(snapshots[3].tx_index, 3);
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
    #[ignore = "requires archive RPC for historical depeg window"]
    fn test_dex_dex_intra_depeg() {
        init_test_tracing();
        let Some(rpc_url) = std::env::var("MEV_RPC_URL").ok() else {
            eprintln!("MEV_RPC_URL not set; skipping DEX-DEX intra depeg test");
            return;
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime should build");

        runtime.block_on(async {
            // Test a range of blocks around the USDC depeg.
            let blocks_to_scan: Vec<u64> = (16_817_000u64..16_817_100u64).collect();
            let mut total_profitable = 0usize;
            let mut total_candidates = 0usize;
            let mut global_max_spread: u128 = 0;
            let mut _total_sync_events = 0usize;
            let mut sample_rows: Vec<IntraBlockArb> = Vec::new();

            for block_number in &blocks_to_scan {
                // Fetch base fee for the block.
                let payload = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", block_number), false],
                });

                let resp = reqwest::Client::new()
                    .post(&rpc_url)
                    .json(&payload)
                    .send()
                    .await;

                let base_fee = match resp {
                    Ok(response) => {
                        let body: serde_json::Value = response.json().await.unwrap_or_default();
                        body.get("result")
                            .and_then(|r| r.get("baseFeePerGas"))
                            .and_then(|v| v.as_str())
                            .and_then(|hex| {
                                u128::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
                            })
                            .unwrap_or(0)
                    }
                    Err(error) => {
                        eprintln!("block {} fetch failed: {}", block_number, error);
                        continue;
                    }
                };

                let results =
                    scan_default_dex_dex_intra_block(&rpc_url, *block_number, base_fee).await;

                match results {
                    Ok(opportunities) => {
                        let profitable_count =
                            opportunities.iter().filter(|o| o.profit_wei > 0).count();
                        let candidate_count = opportunities
                            .iter()
                            .filter(|o| o.profit_wei == 0 && !o.verdict.is_empty())
                            .count();

                        for opp in &opportunities {
                            if opp.spread_bps > global_max_spread {
                                global_max_spread = opp.spread_bps;
                            }
                        }

                        total_profitable += profitable_count;
                        total_candidates += candidate_count;

                        if sample_rows.len() < 10 {
                            for opp in &opportunities {
                                if sample_rows.len() < 10 {
                                    sample_rows.push(opp.clone());
                                }
                            }
                        }

                        eprintln!(
                            "block={} profitable={} candidates={}",
                            block_number, profitable_count, candidate_count,
                        );
                    }
                    Err(error) => {
                        eprintln!("block {} scan error: {}", block_number, error);
                    }
                }
            }

            eprintln!("\n=== DEX-DEX INTRA DEPEG SUMMARY ===");
            eprintln!("Blocks scanned: {}", blocks_to_scan.len());
            eprintln!("Max spread observed: {} bps", global_max_spread);
            eprintln!("Total profitable rows: {}", total_profitable);
            eprintln!("Total candidate rows: {}", total_candidates);
            eprintln!("Sample rows (first 10):");
            for (idx, row) in sample_rows.iter().take(10).enumerate() {
                eprintln!(
                    "  {}. block={} tx={} spread_bps={} profit_wei={} direction={} verdict={}",
                    idx + 1,
                    row.block_number,
                    row.after_tx_index,
                    row.spread_bps,
                    row.profit_wei,
                    row.direction,
                    if row.verdict.is_empty() {
                        "(profitable)"
                    } else {
                        &row.verdict
                    },
                );
            }
        });
    }
}
