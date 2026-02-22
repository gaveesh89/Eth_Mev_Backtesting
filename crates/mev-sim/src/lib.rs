//! mev-sim: REVM-based transaction simulation for MEV analysis.
//!
//! Forks Ethereum state at block N-1 and simulates transactions for block N.
//! Supports sequential execution, atomic bundles with rollback, and state snapshots.

pub mod evm;

pub use evm::{EvmFork, SimResult};
