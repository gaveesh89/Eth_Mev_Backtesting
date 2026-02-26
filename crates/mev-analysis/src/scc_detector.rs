//! SCC-based arbitrage detection using Tarjan's algorithm.
//!
//! Implements the EigenPhi 6-step algorithm:
//! 1. Parse all transfers in a transaction (done by [`crate::transfer_graph`])
//! 2. Build a directed graph (done by [`crate::transfer_graph`])
//! 3. Find strongly connected components (Tarjan's SCC)
//! 4. Locate the closest node to `tx.from`/`tx.to` within each SCC
//! 5. Calculate net token balance for that node (SCC-internal edges only)
//! 6. Positive net balance → classify as arbitrage
//!
//! This detector is **protocol-agnostic**: it does not need to know which
//! DEX or AMM was used. Any transaction with a profitable token cycle
//! (cycle in the transfer graph with positive net) is flagged.
//!
//! ## Why This Catches More Than Reserve-Based Scanning
//!
//! The reserve-based scanner in `mev-sim::strategies::arbitrage` only checks
//! 6 hardcoded V2 pools. This SCC detector catches multi-hop arbs, cross-protocol
//! arbs, and arbs on any venue — because it operates on Transfer events, not
//! pool reserves.

use std::collections::{HashMap, HashSet};

use alloy::primitives::Address;
use petgraph::algo::tarjan_scc;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use crate::transfer_graph::{u256_to_i128, TxTransferGraph};

/// Result of SCC-based arbitrage analysis for one transaction.
#[derive(Debug, Clone)]
pub struct ArbitrageDetection {
    /// Whether the transaction is classified as arbitrage.
    pub is_arbitrage: bool,
    /// The address that profited (closest node to tx.from/tx.to in the SCC).
    pub profiteer: Address,
    /// Net token balances for the profiteer (token → net amount).
    /// Positive = profit, negative = cost.
    pub net_balances: HashMap<Address, i128>,
    /// Tokens with positive net balance (the actual profit).
    pub profit_tokens: Vec<(Address, i128)>,
    /// Number of addresses participating in the cycle.
    pub scc_node_count: usize,
    /// Number of transfer edges within the cycle.
    pub scc_edge_count: usize,
    /// Number of unique tokens transferred within the cycle.
    pub tokens_involved: usize,
}

/// Detect arbitrage in a transaction's transfer graph using Tarjan's SCC algorithm.
///
/// Returns `Some(ArbitrageDetection)` if a profitable cycle is found,
/// `None` if no cycle exists or all cycles are net-negative.
///
/// ## Algorithm
///
/// For each SCC with > 1 node:
/// 1. Check if `tx_from` or `tx_to` is in the SCC (the "closest point")
/// 2. Calculate net balance for that point using **only SCC-internal edges**
/// 3. If any token has positive net balance → arbitrage detected
///
/// External edges (gas payments, fee transfers outside the cycle) are
/// intentionally excluded to avoid false negatives.
pub fn detect_arbitrage(tx_graph: &TxTransferGraph) -> Option<ArbitrageDetection> {
    if tx_graph.graph.node_count() == 0 {
        return None;
    }

    let sccs = tarjan_scc(&tx_graph.graph);

    // Filter to SCCs with > 1 node (single-node SCCs are not cycles)
    let cyclic_sccs: Vec<Vec<NodeIndex>> = sccs.into_iter().filter(|scc| scc.len() > 1).collect();

    if cyclic_sccs.is_empty() {
        return None;
    }

    for scc in &cyclic_sccs {
        let scc_set: HashSet<NodeIndex> = scc.iter().copied().collect();

        // Step 4: Find closest point to tx_from/tx_to within this SCC
        let closest_ix = find_closest_in_scc(tx_graph, &scc_set);
        let closest_ix = match closest_ix {
            Some(ix) => ix,
            None => continue, // Neither tx_from nor tx_to is in this SCC
        };

        let profiteer = tx_graph.graph[closest_ix];

        // Step 5: Calculate net balance using ONLY SCC-internal edges
        let balances = compute_scc_net_balance(tx_graph, &scc_set, closest_ix);

        // Step 6: Check for positive net in any token
        let profit_tokens: Vec<(Address, i128)> = balances
            .iter()
            .filter(|(_, &net)| net > 0)
            .map(|(&token, &net)| (token, net))
            .collect();

        if !profit_tokens.is_empty() {
            return Some(ArbitrageDetection {
                is_arbitrage: true,
                profiteer,
                net_balances: balances,
                profit_tokens,
                scc_node_count: scc.len(),
                scc_edge_count: count_scc_edges(tx_graph, &scc_set),
                tokens_involved: count_scc_tokens(tx_graph, &scc_set),
            });
        }
    }

    None
}

/// Find the node index of `tx_from` or `tx_to` within the SCC.
///
/// Prefers `tx_from` over `tx_to` (the EOA is more likely to be the profiteer).
fn find_closest_in_scc(
    tx_graph: &TxTransferGraph,
    scc_set: &HashSet<NodeIndex>,
) -> Option<NodeIndex> {
    // Check tx_from first
    if let Some(&ix) = tx_graph.addr_to_ix.get(&tx_graph.tx_from) {
        if scc_set.contains(&ix) {
            return Some(ix);
        }
    }

    // Fall back to tx_to
    if let Some(&ix) = tx_graph.addr_to_ix.get(&tx_graph.tx_to) {
        if scc_set.contains(&ix) {
            return Some(ix);
        }
    }

    None
}

/// Compute the net token balance for the closest node, counting ONLY
/// edges where **both** source and target are within the SCC.
///
/// This is critical: edges leaving the SCC (gas tips, protocol fees)
/// must not reduce the profiteer's apparent balance.
fn compute_scc_net_balance(
    tx_graph: &TxTransferGraph,
    scc_set: &HashSet<NodeIndex>,
    closest_ix: NodeIndex,
) -> HashMap<Address, i128> {
    let mut balances: HashMap<Address, i128> = HashMap::new();

    for edge_ref in tx_graph.graph.edge_references() {
        let src = edge_ref.source();
        let tgt = edge_ref.target();

        // Only count edges fully within the SCC
        if !scc_set.contains(&src) || !scc_set.contains(&tgt) {
            continue;
        }

        let weight = edge_ref.weight();
        let amount_i128 = u256_to_i128(weight.amount);

        if tgt == closest_ix {
            // Inflow to profiteer
            *balances.entry(weight.token).or_default() += amount_i128;
        }
        if src == closest_ix {
            // Outflow from profiteer
            *balances.entry(weight.token).or_default() -= amount_i128;
        }
    }

    balances
}

/// Count the number of edges fully within the SCC.
fn count_scc_edges(tx_graph: &TxTransferGraph, scc_set: &HashSet<NodeIndex>) -> usize {
    tx_graph
        .graph
        .edge_references()
        .filter(|e| scc_set.contains(&e.source()) && scc_set.contains(&e.target()))
        .count()
}

/// Count the number of unique tokens transferred within the SCC.
fn count_scc_tokens(tx_graph: &TxTransferGraph, scc_set: &HashSet<NodeIndex>) -> usize {
    tx_graph
        .graph
        .edge_references()
        .filter(|e| scc_set.contains(&e.source()) && scc_set.contains(&e.target()))
        .map(|e| e.weight().token)
        .collect::<HashSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_graph::{TokenTransfer, TxTransferGraph};
    use alloy::primitives::U256;

    fn addr(n: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[19] = n;
        Address::from(bytes)
    }

    fn token(n: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[18] = 0xff;
        bytes[19] = n;
        Address::from(bytes)
    }

    fn mk(from: Address, to: Address, tok: Address, amount: u64, idx: u32) -> TokenTransfer {
        TokenTransfer {
            token: tok,
            from,
            to,
            amount: U256::from(amount),
            log_index: idx,
        }
    }

    #[test]
    fn simple_two_pool_arb_detected() {
        let bot = addr(1);
        let pool_a = addr(2);
        let pool_b = addr(3);
        let usdc = token(1);
        let weth = token(2);

        let transfers = vec![
            mk(bot, pool_a, usdc, 100, 0),
            mk(pool_a, pool_b, weth, 1, 1),
            mk(pool_b, bot, usdc, 105, 2),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, bot, Address::ZERO);
        let result = detect_arbitrage(&graph).expect("should detect arb");

        assert!(result.is_arbitrage);
        assert_eq!(result.profiteer, bot);
        assert_eq!(result.scc_node_count, 3);
        assert_eq!(result.tokens_involved, 2);
        assert!(result
            .profit_tokens
            .iter()
            .any(|(tok, net)| *tok == usdc && *net == 5));
    }

    #[test]
    fn three_hop_arb_detected() {
        let bot = addr(1);
        let p1 = addr(2);
        let p2 = addr(3);
        let p3 = addr(4);
        let usdc = token(1);

        let transfers = vec![
            mk(bot, p1, usdc, 100, 0),
            mk(p1, p2, token(2), 50, 1),
            mk(p2, p3, token(3), 1, 2),
            mk(p3, bot, usdc, 102, 3),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, bot, Address::ZERO);
        let result = detect_arbitrage(&graph).expect("should detect 3-hop arb");

        assert!(result.is_arbitrage);
        assert_eq!(result.scc_node_count, 4);
        assert!(result
            .profit_tokens
            .iter()
            .any(|(tok, net)| *tok == usdc && *net == 2));
    }

    #[test]
    fn no_cycle_no_arb() {
        let a = addr(1);
        let b = addr(2);
        let c = addr(3);

        // Linear chain: A → B → C (no cycle)
        let transfers = vec![mk(a, b, token(1), 100, 0), mk(b, c, token(1), 100, 1)];

        let graph = TxTransferGraph::from_transfers(&transfers, a, Address::ZERO);
        assert!(detect_arbitrage(&graph).is_none());
    }

    #[test]
    fn cycle_but_net_loss() {
        let bot = addr(1);
        let pool_a = addr(2);
        let pool_b = addr(3);
        let usdc = token(1);

        // Bot sends 100, gets back 98 → net loss
        let transfers = vec![
            mk(bot, pool_a, usdc, 100, 0),
            mk(pool_a, pool_b, token(2), 1, 1),
            mk(pool_b, bot, usdc, 98, 2),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, bot, Address::ZERO);
        assert!(
            detect_arbitrage(&graph).is_none(),
            "net-loss cycle should not be classified as arb"
        );
    }

    #[test]
    fn multi_token_profit() {
        let bot = addr(1);
        let p1 = addr(2);
        let p2 = addr(3);
        let usdc = token(1);
        let weth = token(2);

        // USDC breaks even, but bot gains WETH
        let transfers = vec![
            mk(bot, p1, usdc, 100, 0),
            mk(p1, p2, weth, 1, 1),
            mk(p2, bot, usdc, 100, 2),
            mk(p2, bot, weth, 1, 3), // extra WETH profit
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, bot, Address::ZERO);
        let result = detect_arbitrage(&graph).expect("should detect multi-token profit");

        assert!(result.is_arbitrage);
        // WETH should show profit (inflow from p2: 1, outflow: 0 within SCC for bot)
        // Actually bot doesn't send WETH, so WETH net = +1
        assert!(result.profit_tokens.iter().any(|(tok, _)| *tok == weth));
    }

    #[test]
    fn scc_with_external_edges_ignored() {
        let bot = addr(1);
        let pool_a = addr(2);
        let pool_b = addr(3);
        let fee_recipient = addr(99); // NOT in the cycle
        let usdc = token(1);

        // Cycle: bot → pool_a → pool_b → bot (profitable)
        // External: bot → fee_recipient (gas tip)
        let transfers = vec![
            mk(bot, pool_a, usdc, 100, 0),
            mk(pool_a, pool_b, token(2), 1, 1),
            mk(pool_b, bot, usdc, 110, 2),
            mk(bot, fee_recipient, usdc, 5, 3), // external edge
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, bot, Address::ZERO);
        let result =
            detect_arbitrage(&graph).expect("external edge should not block arb detection");

        assert!(result.is_arbitrage);
        // SCC-internal profit: 110 - 100 = +10 USDC (fee_recipient edge excluded)
        assert!(result
            .profit_tokens
            .iter()
            .any(|(tok, net)| *tok == usdc && *net == 10));
    }

    #[test]
    fn profiteer_is_contract_not_eoa() {
        let eoa = addr(99); // NOT in any transfer
        let bot_contract = addr(1);
        let p1 = addr(2);
        let p2 = addr(3);
        let usdc = token(1);

        let transfers = vec![
            mk(bot_contract, p1, usdc, 100, 0),
            mk(p1, p2, token(2), 1, 1),
            mk(p2, bot_contract, usdc, 105, 2),
        ];

        // tx_from = eoa (NOT in graph), tx_to = bot_contract (IN the cycle)
        let graph = TxTransferGraph::from_transfers(&transfers, eoa, bot_contract);
        let result = detect_arbitrage(&graph).expect("should detect arb via tx_to fallback");

        assert!(result.is_arbitrage);
        assert_eq!(result.profiteer, bot_contract);
    }

    #[test]
    fn empty_graph_returns_none() {
        let graph = TxTransferGraph::from_transfers(&[], Address::ZERO, Address::ZERO);
        assert!(detect_arbitrage(&graph).is_none());
    }
}
