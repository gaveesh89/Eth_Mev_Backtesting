//! MEV analysis engine — runs entirely in the browser (WASM).
//!
//! Implements simplified versions of the toolkit's detection algorithms:
//! - Sandwich detection (3-tx sliding window)
//! - Arbitrage detection (cyclic ERC-20 transfers)
//! - Transaction ordering analysis (EGP comparison)
//! - Block-level value computation

use std::collections::{HashMap, HashSet};

use crate::types::*;

// ---------------------------------------------------------------------------
// Well-known event topic0 signatures
// ---------------------------------------------------------------------------

/// ERC-20 Transfer(address,address,uint256)
const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// Uniswap V2 Swap(address,uint256,uint256,uint256,uint256,address)
const V2_SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";

/// Uniswap V3 Swap(address,int256,int256,uint160,int128,int24)
const V3_SWAP_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";

/// Uniswap V2 Sync(uint112,uint112)
const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";

// ---------------------------------------------------------------------------
// Well-known contract addresses (lowercased)
// ---------------------------------------------------------------------------

const WETH: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";

// ---------------------------------------------------------------------------
// Parsed intermediate types
// ---------------------------------------------------------------------------

/// A parsed ERC-20 transfer extracted from a log.
#[derive(Clone, Debug)]
struct TokenTransfer {
    token: String,
    from: String,
    to: String,
    amount: u128,
    log_index: u64,
}

/// A parsed DEX swap event.
#[derive(Clone, Debug)]
struct SwapEvent {
    pool: String,
    tx_hash: String,
    tx_index: u64,
    sender: String, // the tx originator (from field of tx)
    is_v3: bool,
}

// ---------------------------------------------------------------------------
// Core analysis entry point
// ---------------------------------------------------------------------------

/// Run full MEV analysis on a set of blocks + receipts.
///
/// Returns analysis results suitable for display.
pub fn analyze_blocks(
    blocks: &[(RpcBlock, Vec<RpcReceipt>)],
    strategy: &Strategy,
) -> AnalysisResults {
    let mut results = AnalysisResults::default();

    for (block, receipts) in blocks {
        let block_number = block.number.as_deref().map(parse_hex_u64).unwrap_or(0);
        let base_fee = block
            .base_fee_per_gas
            .as_deref()
            .map(parse_hex_u128)
            .unwrap_or(0);
        let gas_used = block.gas_used.as_deref().map(parse_hex_u128).unwrap_or(0);

        let tx_count = block.transactions.len();
        results.transactions_analyzed += tx_count as u64;
        results.blocks_analyzed += 1;

        // Build tx-hash → sender lookup
        let tx_sender: HashMap<String, String> = block
            .transactions
            .iter()
            .filter_map(|tx| {
                let hash = tx.hash.as_deref()?.to_lowercase();
                let from = tx.from.as_deref()?.to_lowercase();
                Some((hash, from))
            })
            .collect();

        // Parse events from receipts
        let transfers = parse_all_transfers(receipts);
        let swaps = parse_all_swaps(receipts, &tx_sender);

        // Total block gas cost
        let block_gas_cost_wei = compute_block_value(&block.transactions, base_fee);
        let block_gas_eth = block_gas_cost_wei as f64 / 1e18;
        results.total_gas_eth += block_gas_eth;

        // --- Strategy-specific analysis ---
        match strategy {
            Strategy::SandwichDetection => {
                let sands = detect_sandwiches(&swaps, block_number);
                results.opportunities.extend(sands);
            }
            Strategy::DexDexArb => {
                let arbs = detect_arbitrages(&transfers, receipts, block_number);
                results.opportunities.extend(arbs);
            }
            Strategy::FullMevScan => {
                let sands = detect_sandwiches(&swaps, block_number);
                let arbs = detect_arbitrages(&transfers, receipts, block_number);
                results.opportunities.extend(sands);
                results.opportunities.extend(arbs);
            }
        }

        // --- Ordering analysis (always) ---
        let ordering = analyze_ordering(&block.transactions, base_fee, block_number);
        results.ordering_analysis.extend(ordering);

        // --- Block summary ---
        results.block_summaries.push(BlockSummary {
            block_number,
            tx_count,
            gas_used,
            base_fee_gwei: base_fee as f64 / 1e9,
            block_value_eth: block_gas_eth,
            mev_count: results
                .opportunities
                .iter()
                .filter(|o| o.block_number == block_number)
                .count(),
        });
    }

    // Sort opportunities by block and estimated profit
    results.opportunities.sort_by(|a, b| {
        a.block_number.cmp(&b.block_number).then(
            b.estimated_profit_eth
                .partial_cmp(&a.estimated_profit_eth)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    results
}

// ---------------------------------------------------------------------------
// Event parsing
// ---------------------------------------------------------------------------

fn parse_all_transfers(receipts: &[RpcReceipt]) -> Vec<TokenTransfer> {
    let mut out = Vec::new();
    for receipt in receipts {
        for log in &receipt.logs {
            if let Some(t) = parse_transfer_log(log) {
                out.push(t);
            }
        }
    }
    out
}

fn parse_transfer_log(log: &RpcLog) -> Option<TokenTransfer> {
    if log.topics.len() < 3 {
        return None;
    }
    let topic0 = log.topics[0].to_lowercase();
    if topic0 != TRANSFER_TOPIC {
        return None;
    }

    let token = log.address.as_deref()?.to_lowercase();
    let from = topic_to_address(&log.topics[1]);
    let to = topic_to_address(&log.topics[2]);
    let amount = parse_hex_u128(log.data.as_deref().unwrap_or("0x0"));
    let log_index = parse_hex_u64(log.log_index.as_deref().unwrap_or("0x0"));

    Some(TokenTransfer {
        token,
        from,
        to,
        amount,
        log_index,
    })
}

fn parse_all_swaps(receipts: &[RpcReceipt], tx_sender: &HashMap<String, String>) -> Vec<SwapEvent> {
    let mut out = Vec::new();
    for receipt in receipts {
        let tx_hash = receipt
            .transaction_hash
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        let tx_index = parse_hex_u64(receipt.transaction_index.as_deref().unwrap_or("0x0"));
        let sender = tx_sender.get(&tx_hash).cloned().unwrap_or_default();

        for log in &receipt.logs {
            if log.topics.is_empty() {
                continue;
            }
            let topic0 = log.topics[0].to_lowercase();
            let is_swap = topic0 == V2_SWAP_TOPIC || topic0 == V3_SWAP_TOPIC;
            if !is_swap {
                continue;
            }
            let pool = log.address.as_deref().unwrap_or("").to_lowercase();
            out.push(SwapEvent {
                pool,
                tx_hash: tx_hash.clone(),
                tx_index,
                sender: sender.clone(),
                is_v3: topic0 == V3_SWAP_TOPIC,
            });
        }
    }
    out.sort_by_key(|s| s.tx_index);
    out
}

// ---------------------------------------------------------------------------
// Sandwich detection
// ---------------------------------------------------------------------------

/// Sliding window of size 3 over swap events in a block.
///
/// Conditions (following the main toolkit's `detect_sandwich_pattern`):
/// 1. tx[i].sender == tx[k].sender (same address wraps)
/// 2. tx[j].sender != tx[i].sender (different victim)
/// 3. Same pool touched by tx[i] and tx[k]
/// 4. tx[i], tx[j], tx[k] are in ascending tx_index order
fn detect_sandwiches(swaps: &[SwapEvent], block_number: u64) -> Vec<MevOpportunity> {
    let mut seen_triples: HashSet<(String, String, String)> = HashSet::new();
    let mut results = Vec::new();

    if swaps.len() < 3 {
        return results;
    }

    for i in 0..swaps.len().saturating_sub(2) {
        for j in (i + 1)..swaps.len().saturating_sub(1) {
            if swaps[j].sender == swaps[i].sender {
                continue; // victim must differ
            }
            for k in (j + 1)..swaps.len() {
                if swaps[k].sender != swaps[i].sender {
                    continue; // backrun must match frontrun sender
                }
                if swaps[k].pool != swaps[i].pool {
                    continue; // same pool
                }

                let triple = (
                    swaps[i].tx_hash.clone(),
                    swaps[j].tx_hash.clone(),
                    swaps[k].tx_hash.clone(),
                );
                if seen_triples.contains(&triple) {
                    continue;
                }
                seen_triples.insert(triple);

                let confidence = compute_sandwich_confidence(&swaps[i], &swaps[j], &swaps[k]);

                results.push(MevOpportunity {
                    block_number,
                    tx_hash: swaps[i].tx_hash.clone(),
                    mev_type: MevType::Sandwich,
                    estimated_profit_eth: 0.0, // would need simulation for exact P&L
                    gas_cost_eth: 0.0,
                    confidence,
                    details: format!(
                        "Frontrun {} → Victim {} → Backrun {} on pool {}",
                        short_hash(&swaps[i].tx_hash),
                        short_hash(&swaps[j].tx_hash),
                        short_hash(&swaps[k].tx_hash),
                        short_hash(&swaps[i].pool),
                    ),
                });
            }
        }
    }

    results
}

fn compute_sandwich_confidence(front: &SwapEvent, _victim: &SwapEvent, back: &SwapEvent) -> f64 {
    let mut score: f64 = 0.0;

    // Same sender for front and back
    if front.sender == back.sender {
        score += 0.35;
    }
    // Same pool
    if front.pool == back.pool {
        score += 0.30;
    }
    // Consecutive or near-consecutive positions
    let gap = back.tx_index.saturating_sub(front.tx_index);
    if gap <= 3 {
        score += 0.20;
    }
    // Both V2 or both V3
    if front.is_v3 == back.is_v3 {
        score += 0.15;
    }

    score.min(1.0)
}

// ---------------------------------------------------------------------------
// Arbitrage detection (cyclic ERC-20 transfers)
// ---------------------------------------------------------------------------

/// Group transfers by transaction, then check for cycles.
fn detect_arbitrages(
    transfers: &[TokenTransfer],
    receipts: &[RpcReceipt],
    block_number: u64,
) -> Vec<MevOpportunity> {
    // Group transfers by tx hash via receipt lookup
    // Build log_index → tx_hash mapping
    let mut log_to_tx: HashMap<u64, String> = HashMap::new();
    for receipt in receipts {
        let tx_hash = receipt
            .transaction_hash
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        for log in &receipt.logs {
            let idx = parse_hex_u64(log.log_index.as_deref().unwrap_or("0x0"));
            log_to_tx.insert(idx, tx_hash.clone());
        }
    }

    // Group transfers by tx
    let mut by_tx: HashMap<String, Vec<&TokenTransfer>> = HashMap::new();
    for t in transfers {
        if let Some(tx_hash) = log_to_tx.get(&t.log_index) {
            by_tx.entry(tx_hash.clone()).or_default().push(t);
        }
    }

    let mut results = Vec::new();

    for (tx_hash, tx_transfers) in &by_tx {
        if tx_transfers.len() < 2 {
            continue;
        }

        // Build address net-balance: for each address, sum token inflows minus outflows
        let mut net: HashMap<String, HashMap<String, i128>> = HashMap::new();
        for t in tx_transfers {
            *net.entry(t.from.clone())
                .or_default()
                .entry(t.token.clone())
                .or_insert(0) -= t.amount as i128;
            *net.entry(t.to.clone())
                .or_default()
                .entry(t.token.clone())
                .or_insert(0) += t.amount as i128;
        }

        // Look for addresses with positive net balance in any token
        // that also has negative net in another token (classic arb pattern)
        for (addr, balances) in &net {
            let positives: Vec<_> = balances.iter().filter(|(_, &v)| v > 0).collect();
            let negatives: Vec<_> = balances.iter().filter(|(_, &v)| v < 0).collect();

            if positives.is_empty() || negatives.is_empty() {
                continue;
            }

            // Check if the positive token is valuable (WETH or known stable)
            let profit_token = positives[0].0.to_lowercase();
            let profit_amount = *positives[0].1;

            // Estimate profit in ETH
            let profit_eth = if profit_token == WETH {
                profit_amount as f64 / 1e18
            } else {
                // Can't price non-WETH tokens without oracle; show as token units
                profit_amount as f64 / 1e18
            };

            if profit_eth.abs() < 1e-12 {
                continue;
            }

            results.push(MevOpportunity {
                block_number,
                tx_hash: tx_hash.clone(),
                mev_type: MevType::Arbitrage,
                estimated_profit_eth: profit_eth,
                gas_cost_eth: 0.0,
                confidence: 0.75,
                details: format!(
                    "Cyclic flow: {} gained {} of token {} across {} transfers",
                    short_hash(addr),
                    format_profit(profit_amount),
                    short_hash(&profit_token),
                    tx_transfers.len(),
                ),
            });

            break; // one finding per tx
        }
    }

    // Deduplicate by tx_hash
    let mut seen = HashSet::new();
    results.retain(|o| seen.insert(o.tx_hash.clone()));

    results
}

fn format_profit(amount: i128) -> String {
    let eth = amount.unsigned_abs() as f64 / 1e18;
    if amount >= 0 {
        format!("+{eth:.6} ETH")
    } else {
        format!("-{eth:.6} ETH")
    }
}

// ---------------------------------------------------------------------------
// Transaction ordering analysis
// ---------------------------------------------------------------------------

fn analyze_ordering(
    txs: &[RpcTransaction],
    base_fee: u128,
    block_number: u64,
) -> Vec<OrderingInsight> {
    // Calculate EGP for each transaction
    let egp_list: Vec<(usize, String, u128)> = txs
        .iter()
        .enumerate()
        .map(|(idx, tx)| {
            let hash = tx.hash.as_deref().unwrap_or("").to_string();
            let egp = calculate_egp(tx, base_fee);
            (idx, hash, egp)
        })
        .collect();

    // Optimal ordering: sort by EGP descending
    let mut optimal = egp_list.clone();
    optimal.sort_by(|a, b| b.2.cmp(&a.2));

    // Build optimal position map
    let optimal_pos: HashMap<String, usize> = optimal
        .iter()
        .enumerate()
        .map(|(pos, (_, hash, _))| (hash.clone(), pos))
        .collect();

    // Only report misordered transactions (top 20 most misordered)
    let mut insights: Vec<OrderingInsight> = egp_list
        .iter()
        .filter_map(|(actual, hash, egp)| {
            let opt = optimal_pos.get(hash).copied().unwrap_or(*actual);
            let misordered = actual.abs_diff(opt) > 0;
            Some(OrderingInsight {
                block_number,
                tx_hash: hash.clone(),
                actual_position: *actual,
                optimal_position: opt,
                egp_gwei: *egp as f64 / 1e9,
                is_misordered: misordered,
            })
        })
        .collect();

    // Keep only top misordered ones for display
    insights.sort_by(|a, b| {
        let diff_a = a.actual_position.abs_diff(a.optimal_position);
        let diff_b = b.actual_position.abs_diff(b.optimal_position);
        diff_b.cmp(&diff_a)
    });
    insights.truncate(20);

    insights
}

/// Calculate effective gas price (EGP) for a transaction.
fn calculate_egp(tx: &RpcTransaction, base_fee: u128) -> u128 {
    let tx_type = parse_hex_u64(tx.tx_type.as_deref().unwrap_or("0x0"));

    if tx_type == 2 {
        // EIP-1559
        let max_fee = parse_hex_u128(tx.max_fee_per_gas.as_deref().unwrap_or("0x0"));
        let max_priority = parse_hex_u128(tx.max_priority_fee_per_gas.as_deref().unwrap_or("0x0"));
        max_fee.min(base_fee.saturating_add(max_priority))
    } else {
        // Legacy
        parse_hex_u128(tx.gas_price.as_deref().unwrap_or("0x0"))
    }
}

// ---------------------------------------------------------------------------
// Block value computation
// ---------------------------------------------------------------------------

/// Compute total block value: sum of (EGP - base_fee) * gas_used per tx.
fn compute_block_value(txs: &[RpcTransaction], base_fee: u128) -> u128 {
    txs.iter()
        .map(|tx| {
            let egp = calculate_egp(tx, base_fee);
            let gas = parse_hex_u128(tx.gas.as_deref().unwrap_or("0x0"));
            egp.saturating_sub(base_fee).saturating_mul(gas)
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the last 20 bytes of a 32-byte topic as a checksumless address.
fn topic_to_address(topic: &str) -> String {
    let hex = topic.strip_prefix("0x").unwrap_or(topic);
    if hex.len() >= 40 {
        format!("0x{}", &hex[hex.len() - 40..]).to_lowercase()
    } else {
        format!("0x{hex}").to_lowercase()
    }
}

/// Truncate a hex hash for display: "0xab12…ef56"
fn short_hash(s: &str) -> String {
    if s.len() > 12 {
        format!("{}…{}", &s[..6], &s[s.len() - 4..])
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_to_address() {
        let topic = "0x0000000000000000000000001234567890abcdef1234567890abcdef12345678";
        let addr = topic_to_address(topic);
        assert_eq!(addr, "0x1234567890abcdef1234567890abcdef12345678");
    }

    #[test]
    fn test_short_hash() {
        let hash = "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let short = short_hash(hash);
        assert!(short.starts_with("0xabcd"));
        assert!(short.contains('…'));
    }

    #[test]
    fn test_calculate_egp_legacy() {
        let tx = RpcTransaction {
            hash: Some("0x1".into()),
            from: None,
            to: None,
            value: None,
            gas: Some("0x5208".into()),
            gas_price: Some("0x3b9aca00".into()), // 1 gwei
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            input: None,
            nonce: None,
            transaction_index: None,
            tx_type: Some("0x0".into()),
        };
        let egp = calculate_egp(&tx, 500_000_000);
        assert_eq!(egp, 1_000_000_000); // 1 gwei
    }

    #[test]
    fn test_calculate_egp_eip1559() {
        let tx = RpcTransaction {
            hash: Some("0x1".into()),
            from: None,
            to: None,
            value: None,
            gas: Some("0x5208".into()),
            gas_price: None,
            max_fee_per_gas: Some("0x77359400".into()), // 2 gwei
            max_priority_fee_per_gas: Some("0x3b9aca00".into()), // 1 gwei
            input: None,
            nonce: None,
            transaction_index: None,
            tx_type: Some("0x2".into()),
        };
        // base_fee = 0.5 gwei
        let egp = calculate_egp(&tx, 500_000_000);
        // min(2 gwei, 0.5 + 1 = 1.5 gwei) = 1.5 gwei
        assert_eq!(egp, 1_500_000_000);
    }

    #[test]
    fn test_format_profit_positive() {
        let s = format_profit(1_000_000_000_000_000_000); // 1 ETH
        assert!(s.contains("+1.000000"));
    }

    #[test]
    fn test_sandwich_confidence_full_match() {
        let front = SwapEvent {
            pool: "0xpool".into(),
            tx_hash: "0x1".into(),
            tx_index: 0,
            sender: "0xattacker".into(),
            is_v3: false,
        };
        let victim = SwapEvent {
            pool: "0xpool".into(),
            tx_hash: "0x2".into(),
            tx_index: 1,
            sender: "0xvictim".into(),
            is_v3: false,
        };
        let back = SwapEvent {
            pool: "0xpool".into(),
            tx_hash: "0x3".into(),
            tx_index: 2,
            sender: "0xattacker".into(),
            is_v3: false,
        };
        let conf = compute_sandwich_confidence(&front, &victim, &back);
        assert!(conf >= 0.95, "confidence should be ≥ 0.95, got {conf}");
    }
}
