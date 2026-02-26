//! Transfer graph construction from ERC-20 Transfer event logs.
//!
//! Builds a directed multi-edge graph where nodes are Ethereum addresses
//! and edges represent token transfers. This is the foundation for
//! SCC-based arbitrage detection (see [`crate::scc_detector`]).
//!
//! ## Design
//!
//! EigenPhi's core insight: represent every transaction as a **collection
//! of asset transfers**, then use graph-theoretic rules (strongly connected
//! components) to classify MEV. This module implements the graph construction
//! phase of that pipeline.
//!
//! Parallel edges are intentional — a single transaction can contain
//! multiple Transfer events between the same address pair (different tokens
//! or multiple transfers of the same token).

use std::collections::HashMap;

use alloy::primitives::{Address, B256, U256};
use mev_data::types::TxLog;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;

/// ERC-20 Transfer event signature: `keccak256("Transfer(address,address,uint256)")`.
pub const ERC20_TRANSFER_TOPIC0: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// A single decoded ERC-20 transfer from tx receipt logs.
#[derive(Debug, Clone)]
pub struct TokenTransfer {
    /// ERC-20 contract address (the log emitter).
    pub token: Address,
    /// Sender of the token transfer.
    pub from: Address,
    /// Recipient of the token transfer.
    pub to: Address,
    /// Amount transferred.
    pub amount: U256,
    /// Log index for ordering within the transaction.
    pub log_index: u32,
}

/// Edge weight in the transfer graph: one token transfer between two addresses.
#[derive(Debug, Clone)]
pub struct TransferEdge {
    /// Token contract address.
    pub token: Address,
    /// Amount transferred.
    pub amount: U256,
    /// Log index for ordering.
    pub log_index: u32,
}

/// Directed multi-edge graph for a single transaction's token flows.
///
/// Nodes are Ethereum addresses. Edges are ERC-20 Transfer events.
/// Multiple edges between the same node pair are expected (different tokens).
pub struct TxTransferGraph {
    /// The underlying petgraph directed graph.
    pub graph: DiGraph<Address, TransferEdge>,
    /// Lookup from address to node index.
    pub addr_to_ix: HashMap<Address, NodeIndex>,
    /// The EOA that sent the transaction (tx.from).
    pub tx_from: Address,
    /// The contract called by the transaction (tx.to).
    pub tx_to: Address,
}

impl TxTransferGraph {
    /// Build a transfer graph from decoded token transfers.
    ///
    /// Uses `add_edge` (not `update_edge`) to preserve parallel edges.
    pub fn from_transfers(transfers: &[TokenTransfer], tx_from: Address, tx_to: Address) -> Self {
        let mut graph = DiGraph::new();
        let mut addr_to_ix: HashMap<Address, NodeIndex> = HashMap::new();

        for transfer in transfers {
            let from_ix = *addr_to_ix
                .entry(transfer.from)
                .or_insert_with(|| graph.add_node(transfer.from));
            let to_ix = *addr_to_ix
                .entry(transfer.to)
                .or_insert_with(|| graph.add_node(transfer.to));

            graph.add_edge(
                from_ix,
                to_ix,
                TransferEdge {
                    token: transfer.token,
                    amount: transfer.amount,
                    log_index: transfer.log_index,
                },
            );
        }

        Self {
            graph,
            addr_to_ix,
            tx_from,
            tx_to,
        }
    }

    /// Compute the net token balance for a given node.
    ///
    /// Iterates ALL edges in the graph:
    /// - Inflow (edge target == node): adds the amount
    /// - Outflow (edge source == node): subtracts the amount
    ///
    /// Returns a map of `token_address → net_balance`.
    /// Positive values indicate profit; negative values indicate loss.
    pub fn node_net_balance(&self, node: NodeIndex) -> HashMap<Address, i128> {
        let mut balances: HashMap<Address, i128> = HashMap::new();

        for edge_ref in self.graph.edge_references() {
            let weight = edge_ref.weight();
            let amount_i128 = u256_to_i128(weight.amount);

            if edge_ref.target() == node {
                *balances.entry(weight.token).or_default() += amount_i128;
            }
            if edge_ref.source() == node {
                *balances.entry(weight.token).or_default() -= amount_i128;
            }
        }

        balances
    }

    /// Find the closest entry-point node for MEV analysis.
    ///
    /// Returns the node index for `tx_from` if present in the graph,
    /// otherwise falls back to `tx_to`. Returns `None` if neither
    /// is in the graph.
    pub fn find_closest_node(&self) -> Option<NodeIndex> {
        self.addr_to_ix
            .get(&self.tx_from)
            .or_else(|| self.addr_to_ix.get(&self.tx_to))
            .copied()
    }
}

/// Parse a [`TxLog`] into a [`TokenTransfer`] if it is an ERC-20 Transfer event.
///
/// Returns `None` for non-Transfer events or logs with missing/malformed fields.
pub fn parse_transfer_log(log: &TxLog) -> Option<TokenTransfer> {
    if log.topic0 != ERC20_TRANSFER_TOPIC0 {
        return None;
    }
    let topic1 = log.topic1.as_ref()?;
    let topic2 = log.topic2.as_ref()?;

    let from = parse_address_from_topic(topic1)?;
    let to = parse_address_from_topic(topic2)?;
    let amount = parse_u256_from_hex(&log.data)?;
    let token = log.address.parse::<Address>().ok()?;

    Some(TokenTransfer {
        token,
        from,
        to,
        amount,
        log_index: log.log_index as u32,
    })
}

/// Parse an Ethereum address from a 32-byte topic hex string.
///
/// Addresses are right-aligned in 32-byte topics: bytes 12..32 hold the address.
fn parse_address_from_topic(topic_hex: &str) -> Option<Address> {
    let topic = topic_hex.parse::<B256>().ok()?;
    Some(Address::from_slice(&topic[12..]))
}

/// Parse a U256 from a hex-encoded data field.
///
/// Handles both `0x`-prefixed and bare hex. Takes the first 64 hex chars (32 bytes).
fn parse_u256_from_hex(data_hex: &str) -> Option<U256> {
    let hex = data_hex.strip_prefix("0x").unwrap_or(data_hex);
    if hex.is_empty() {
        return Some(U256::ZERO);
    }
    let trimmed = &hex[..hex.len().min(64)];
    let padded = format!("0x{trimmed:0>64}");
    padded.parse::<U256>().ok()
}

/// Convert a U256 to i128, capping at i128::MAX for overflow safety.
pub fn u256_to_i128(value: U256) -> i128 {
    let as_u128: u128 = value.try_into().unwrap_or(u128::MAX);
    i128::try_from(as_u128).unwrap_or(i128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn mk_transfer(
        from: Address,
        to: Address,
        tok: Address,
        amount: u64,
        idx: u32,
    ) -> TokenTransfer {
        TokenTransfer {
            token: tok,
            from,
            to,
            amount: U256::from(amount),
            log_index: idx,
        }
    }

    #[test]
    fn simple_triangle_arbitrage() {
        let a = addr(1);
        let b = addr(2);
        let c = addr(3);
        let usdc = token(1);

        let transfers = vec![
            mk_transfer(a, b, usdc, 100, 0),
            mk_transfer(b, c, token(2), 1, 1),
            mk_transfer(c, a, usdc, 101, 2),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, a, Address::ZERO);
        assert_eq!(graph.graph.node_count(), 3);
        assert_eq!(graph.graph.edge_count(), 3);

        let a_ix = graph.addr_to_ix[&a];
        let balances = graph.node_net_balance(a_ix);
        // A: received 101 USDC, sent 100 USDC → net +1 USDC
        assert_eq!(balances[&usdc], 1);
    }

    #[test]
    fn parallel_edges_different_tokens() {
        let a = addr(1);
        let b = addr(2);

        let transfers = vec![
            mk_transfer(a, b, token(1), 100, 0),
            mk_transfer(a, b, token(2), 50, 1),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, a, Address::ZERO);
        assert_eq!(graph.graph.node_count(), 2);
        assert_eq!(graph.graph.edge_count(), 2); // parallel edges preserved
    }

    #[test]
    fn no_transfers_empty_graph() {
        let graph = TxTransferGraph::from_transfers(&[], Address::ZERO, Address::ZERO);
        assert_eq!(graph.graph.node_count(), 0);
        assert_eq!(graph.graph.edge_count(), 0);
    }

    #[test]
    fn complex_multi_hop() {
        let a = addr(1);
        let b = addr(2);
        let c = addr(3);
        let d = addr(4);
        let e = addr(5);
        let usdc = token(1);

        let transfers = vec![
            mk_transfer(a, b, usdc, 100, 0),
            mk_transfer(b, c, token(2), 1, 1),
            mk_transfer(c, d, token(3), 200, 2),
            mk_transfer(d, a, usdc, 103, 3),
            mk_transfer(a, e, usdc, 2, 4),
        ];

        let graph = TxTransferGraph::from_transfers(&transfers, a, Address::ZERO);
        assert_eq!(graph.graph.node_count(), 5);
        assert_eq!(graph.graph.edge_count(), 5);

        let a_ix = graph.addr_to_ix[&a];
        let balances = graph.node_net_balance(a_ix);
        // A: 103 in - 100 out - 2 out = +1 USDC
        assert_eq!(balances[&usdc], 1);
    }

    #[test]
    fn find_closest_prefers_tx_from() {
        let a = addr(1);
        let b = addr(2);
        let transfers = vec![mk_transfer(a, b, token(1), 100, 0)];

        let graph = TxTransferGraph::from_transfers(&transfers, a, b);
        let closest = graph.find_closest_node().unwrap();
        // tx_from (a) is in the graph → should be selected
        assert_eq!(graph.graph[closest], a);
    }

    #[test]
    fn find_closest_falls_back_to_tx_to() {
        let a = addr(1);
        let b = addr(2);
        let external = addr(99);
        let transfers = vec![mk_transfer(a, b, token(1), 100, 0)];

        let graph = TxTransferGraph::from_transfers(&transfers, external, a);
        let closest = graph.find_closest_node().unwrap();
        // tx_from (external) NOT in graph → falls back to tx_to (a)
        assert_eq!(graph.graph[closest], a);
    }

    #[test]
    fn find_closest_returns_none_when_neither_present() {
        let a = addr(1);
        let b = addr(2);
        let ext1 = addr(98);
        let ext2 = addr(99);
        let transfers = vec![mk_transfer(a, b, token(1), 100, 0)];

        let graph = TxTransferGraph::from_transfers(&transfers, ext1, ext2);
        assert!(graph.find_closest_node().is_none());
    }

    #[test]
    fn parse_transfer_log_valid() {
        let log = TxLog {
            block_number: 100,
            tx_hash: "0xaabb".to_string(),
            tx_index: 0,
            log_index: 0,
            address: "0x00000000000000000000000000000000000000ff".to_string(),
            topic0: ERC20_TRANSFER_TOPIC0.to_string(),
            topic1: Some(
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            ),
            topic2: Some(
                "0x0000000000000000000000000000000000000000000000000000000000000002".to_string(),
            ),
            topic3: None,
            data: "0x0000000000000000000000000000000000000000000000000000000000000064".to_string(),
        };

        let transfer = parse_transfer_log(&log).expect("should parse valid Transfer log");
        assert_eq!(transfer.from, addr(1));
        assert_eq!(transfer.to, addr(2));
        assert_eq!(transfer.amount, U256::from(100));
    }

    #[test]
    fn parse_transfer_log_rejects_non_transfer() {
        let log = TxLog {
            block_number: 100,
            tx_hash: "0xaabb".to_string(),
            tx_index: 0,
            log_index: 0,
            address: "0x0000000000000000000000000000000000000001".to_string(),
            topic0: "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
                .to_string(), // Swap, not Transfer
            topic1: None,
            topic2: None,
            topic3: None,
            data: "0x".to_string(),
        };

        assert!(parse_transfer_log(&log).is_none());
    }

    #[test]
    fn parse_u256_from_hex_cases() {
        assert_eq!(parse_u256_from_hex("0x"), Some(U256::ZERO));
        assert_eq!(parse_u256_from_hex(""), Some(U256::ZERO));
        assert_eq!(
            parse_u256_from_hex(
                "0x0000000000000000000000000000000000000000000000000000000000000064"
            ),
            Some(U256::from(100))
        );
        assert_eq!(parse_u256_from_hex("0x1"), Some(U256::from(1)));
    }
}
