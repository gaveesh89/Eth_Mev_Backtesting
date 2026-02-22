//! Heuristic MEV classification utilities.
//!
//! # Ethical Context
//! Sandwich-pattern detection in this project is for educational replay and
//! historical analysis only. It is used to understand market microstructure,
//! quantify harmful flow, and evaluate mitigation strategies. This crate does
//! not submit transactions or execute live trading behavior.

use std::collections::HashSet;

use mev_data::types::BlockTransaction;
use mev_sim::decoder::{DecodedTx, SwapDirection};

/// Disclaimer shown alongside classifier outputs.
pub const ACCURACY_DISCLAIMER: &str =
    "Heuristic classification. ~80% recall. Compare against EigenPhi for ground truth.";

/// Classified MEV type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MevType {
    /// Sandwich pattern around a victim transaction.
    Sandwich,
    /// Backrun pattern.
    Backrun,
    /// Arbitrage pattern.
    Arbitrage,
    /// Unknown / unclassified pattern.
    Unknown,
}

/// Classification output for one detected pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct MevClassification {
    /// MEV class assigned by heuristic.
    pub mev_type: MevType,
    /// Involved transaction hashes in execution order.
    pub tx_hashes: Vec<String>,
    /// Confidence score in [0, 1].
    pub confidence: f64,
    /// Free-text rationale for the decision.
    pub rationale: String,
}

fn parse_u128_any(value: &str) -> u128 {
    let trimmed = value.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
        return u128::from_str_radix(hex, 16).unwrap_or(0);
    }
    trimmed.parse::<u128>().unwrap_or(0)
}

fn tx_is_swap(decoded: &DecodedTx) -> bool {
    !matches!(decoded, DecodedTx::Unknown)
}

fn tx_is_buy(decoded: &DecodedTx) -> bool {
    match decoded {
        DecodedTx::V2Swap(data) => matches!(data.direction, SwapDirection::EthToToken),
        DecodedTx::V3Swap(data) => matches!(data.direction, SwapDirection::EthToToken),
        DecodedTx::Unknown => false,
    }
}

fn tx_is_sell(decoded: &DecodedTx) -> bool {
    match decoded {
        DecodedTx::V2Swap(data) => matches!(data.direction, SwapDirection::TokenToEth),
        DecodedTx::V3Swap(data) => matches!(data.direction, SwapDirection::TokenToEth),
        DecodedTx::Unknown => false,
    }
}

/// Detect sandwich patterns using the spec heuristic.
///
/// Conditions checked on each 3-transaction rolling window:
/// 1. tx[0] and tx[2] same sender; tx[1] different sender
/// 2. tx[0] buys token X, tx[1] swaps token X, tx[2] sells token X
/// 3. tx[0] and tx[2] have higher effective gas price than tx[1]
///
/// Confidence:
/// - `0.9` if all three conditions are met
/// - `0.6` if exactly two of three conditions are met
pub fn detect_sandwich_pattern(
    txs: &[BlockTransaction],
    decoded: &[DecodedTx],
) -> Vec<MevClassification> {
    let n = txs.len().min(decoded.len());
    if n < 3 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=(n - 3) {
        let t0 = &txs[i];
        let t1 = &txs[i + 1];
        let t2 = &txs[i + 2];

        let d0 = &decoded[i];
        let d1 = &decoded[i + 1];
        let d2 = &decoded[i + 2];

        let cond_sender = t0.from_address == t2.from_address && t1.from_address != t0.from_address;
        let cond_flow = tx_is_buy(d0) && tx_is_swap(d1) && tx_is_sell(d2);

        let gp0 = parse_u128_any(&t0.effective_gas_price);
        let gp1 = parse_u128_any(&t1.effective_gas_price);
        let gp2 = parse_u128_any(&t2.effective_gas_price);
        let cond_priority = gp0 > gp1 && gp2 > gp1;

        let met_count = [cond_sender, cond_flow, cond_priority]
            .iter()
            .filter(|flag| **flag)
            .count();

        let confidence = match met_count {
            3 => 0.9,
            2 => 0.6,
            _ => continue,
        };

        out.push(MevClassification {
            mev_type: MevType::Sandwich,
            tx_hashes: vec![t0.tx_hash.clone(), t1.tx_hash.clone(), t2.tx_hash.clone()],
            confidence,
            rationale: format!(
                "sandwich heuristic: sender={}, flow={}, priority={}",
                cond_sender, cond_flow, cond_priority
            ),
        });
    }

    out
}

fn overlaps(lhs: &[String], rhs: &[String]) -> bool {
    let set: HashSet<&String> = lhs.iter().collect();
    rhs.iter().any(|tx| set.contains(tx))
}

/// Runs all classifiers and deduplicates overlapping classifications.
///
/// Current classifiers:
/// - Sandwich
pub fn classify_all(
    block_txs: &[BlockTransaction],
    decoded: &[DecodedTx],
) -> Vec<MevClassification> {
    let mut all = detect_sandwich_pattern(block_txs, decoded);
    all.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut deduped: Vec<MevClassification> = Vec::new();
    'outer: for candidate in all {
        for existing in &deduped {
            if overlaps(&candidate.tx_hashes, &existing.tx_hashes) {
                continue 'outer;
            }
        }
        deduped.push(candidate);
    }

    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn mk_tx(hash: &str, from: &str, gas_price_hex: &str) -> BlockTransaction {
        BlockTransaction {
            block_number: 1,
            tx_hash: hash.to_string(),
            tx_index: 0,
            from_address: from.to_string(),
            to_address: "0xrouter".to_string(),
            gas_used: 21_000,
            effective_gas_price: gas_price_hex.to_string(),
            status: 1,
        }
    }

    #[test]
    fn sandwich_detects_with_three_conditions() {
        let txs = vec![
            mk_tx("0xa", "0xattacker", "0x64"),
            mk_tx("0xb", "0xvictim", "0x32"),
            mk_tx("0xc", "0xattacker", "0x64"),
        ];

        let decoded = vec![
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::EthToToken,
            }),
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::TokenToToken,
            }),
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::TokenToEth,
            }),
        ];

        let out = detect_sandwich_pattern(&txs, &decoded);
        assert_eq!(out.len(), 1);
        assert!((out[0].confidence - 0.9).abs() < 1e-12);
    }

    #[test]
    fn classify_all_dedupes_overlap() {
        let txs = vec![
            mk_tx("0xa", "0xattacker", "0x64"),
            mk_tx("0xb", "0xvictim", "0x32"),
            mk_tx("0xc", "0xattacker", "0x64"),
            mk_tx("0xd", "0xother", "0x64"),
        ];

        let decoded = vec![
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::EthToToken,
            }),
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::TokenToToken,
            }),
            DecodedTx::V2Swap(mev_sim::decoder::V2SwapData {
                amount_in: U256::ZERO,
                amount_out_min: U256::ZERO,
                path: vec![],
                recipient: alloy::primitives::Address::ZERO,
                deadline: 0,
                direction: SwapDirection::TokenToEth,
            }),
            DecodedTx::Unknown,
        ];

        let out = classify_all(&txs, &decoded);
        assert_eq!(out.len(), 1);
    }
}
