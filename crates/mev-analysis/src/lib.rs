//! mev-analysis crate
//!
//! Post-simulation analytics: MEV classification, P&L computation,
//! transfer graph construction, and SCC-based arbitrage detection.

pub mod classify;
pub mod pnl;
pub mod scc_detector;
pub mod transfer_graph;
