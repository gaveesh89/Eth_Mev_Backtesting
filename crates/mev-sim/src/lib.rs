//! mev-sim: REVM-based transaction simulation for MEV analysis.
//!
//! Forks Ethereum state at block N-1 and simulates transactions for block N.
//! Supports sequential execution, atomic bundles with rollback, and state snapshots.

pub mod decoder;
pub mod evm;
pub mod ordering;
pub mod strategies;

pub use evm::{EvmFork, SimResult};
pub use ordering::{OrderedBlock, OrderingAlgorithm};
