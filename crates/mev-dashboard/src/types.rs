//! Shared types for the MEV dashboard.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// User-facing configuration collected in the Setup page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub rpc_url: String,
    pub start_block: u64,
    pub end_block: u64,
    pub strategy: Strategy,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            start_block: 16_817_000,
            end_block: 16_817_009,
            strategy: Strategy::FullMevScan,
        }
    }
}

/// Analysis strategy the user selects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    DexDexArb,
    SandwichDetection,
    FullMevScan,
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DexDexArb => write!(f, "DEX-DEX Arbitrage"),
            Self::SandwichDetection => write!(f, "Sandwich Detection"),
            Self::FullMevScan => write!(f, "Full MEV Scan"),
        }
    }
}

// ---------------------------------------------------------------------------
// Preset block ranges
// ---------------------------------------------------------------------------

/// A preset block range that users can select.
#[derive(Clone, Debug)]
pub struct BlockPreset {
    pub label: &'static str,
    pub description: &'static str,
    pub start: u64,
    pub end: u64,
}

pub const PRESETS: &[BlockPreset] = &[
    BlockPreset {
        label: "USDC Depeg",
        description: "March 2023 — high DEX activity during USDC depeg event",
        start: 16_817_000,
        end: 16_817_099,
    },
    BlockPreset {
        label: "The Merge",
        description: "Sept 2022 — first PoS blocks with MEV-Boost",
        start: 15_537_394,
        end: 15_537_410,
    },
    BlockPreset {
        label: "High Gas",
        description: "May 2023 — memecoin frenzy, extreme gas prices",
        start: 17_192_000,
        end: 17_192_015,
    },
    BlockPreset {
        label: "Quick Test",
        description: "10 blocks — fast validation run",
        start: 18_000_000,
        end: 18_000_009,
    },
];

// ---------------------------------------------------------------------------
// RPC response types (deserialized from JSON-RPC)
// ---------------------------------------------------------------------------

/// Full block with transaction objects.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlock {
    pub number: Option<String>,
    pub hash: Option<String>,
    pub timestamp: Option<String>,
    pub base_fee_per_gas: Option<String>,
    pub gas_used: Option<String>,
    pub gas_limit: Option<String>,
    pub miner: Option<String>,
    #[serde(default)]
    pub transactions: Vec<RpcTransaction>,
}

/// Transaction object inside a block.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTransaction {
    pub hash: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub value: Option<String>,
    pub gas: Option<String>,
    pub gas_price: Option<String>,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    pub input: Option<String>,
    pub nonce: Option<String>,
    pub transaction_index: Option<String>,
    #[serde(rename = "type")]
    pub tx_type: Option<String>,
}

/// Transaction receipt.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    pub transaction_hash: Option<String>,
    pub transaction_index: Option<String>,
    pub gas_used: Option<String>,
    pub effective_gas_price: Option<String>,
    pub status: Option<String>,
    #[serde(default)]
    pub logs: Vec<RpcLog>,
}

/// A single event log.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    pub data: Option<String>,
    pub log_index: Option<String>,
    pub transaction_index: Option<String>,
    pub transaction_hash: Option<String>,
}

// ---------------------------------------------------------------------------
// Analysis results
// ---------------------------------------------------------------------------

/// Complete analysis output shown in the Results page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AnalysisResults {
    pub blocks_analyzed: u64,
    pub transactions_analyzed: u64,
    pub total_gas_eth: f64,
    pub opportunities: Vec<MevOpportunity>,
    pub ordering_analysis: Vec<OrderingInsight>,
    pub block_summaries: Vec<BlockSummary>,
}

/// A single detected MEV opportunity.
#[derive(Clone, Debug, Serialize)]
pub struct MevOpportunity {
    pub block_number: u64,
    pub tx_hash: String,
    pub mev_type: MevType,
    pub estimated_profit_eth: f64,
    pub gas_cost_eth: f64,
    pub confidence: f64,
    pub details: String,
}

/// MEV category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum MevType {
    Arbitrage,
    Sandwich,
    Liquidation,
    Unknown,
}

impl std::fmt::Display for MevType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arbitrage => write!(f, "Arbitrage"),
            Self::Sandwich => write!(f, "Sandwich"),
            Self::Liquidation => write!(f, "Liquidation"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Per-block statistics.
#[derive(Clone, Debug, Serialize)]
pub struct BlockSummary {
    pub block_number: u64,
    pub tx_count: usize,
    pub gas_used: u128,
    pub base_fee_gwei: f64,
    pub block_value_eth: f64,
    pub mev_count: usize,
}

/// Transaction ordering insight.
#[derive(Clone, Debug, Serialize)]
pub struct OrderingInsight {
    pub block_number: u64,
    pub tx_hash: String,
    pub actual_position: usize,
    pub optimal_position: usize,
    pub egp_gwei: f64,
    pub is_misordered: bool,
}

// ---------------------------------------------------------------------------
// Hex parsing utilities
// ---------------------------------------------------------------------------

/// Parse a hex string (with optional 0x prefix) to u64.
pub fn parse_hex_u64(hex: &str) -> u64 {
    let s = hex.strip_prefix("0x").unwrap_or(hex);
    if s.is_empty() {
        return 0;
    }
    u64::from_str_radix(s, 16).unwrap_or(0)
}

/// Parse a hex string (with optional 0x prefix) to u128.
pub fn parse_hex_u128(hex: &str) -> u128 {
    let s = hex.strip_prefix("0x").unwrap_or(hex);
    if s.is_empty() {
        return 0;
    }
    u128::from_str_radix(s, 16).unwrap_or(0)
}

/// Format Wei as ETH with 6 decimal places.
pub fn format_eth(wei: u128) -> String {
    let eth = wei as f64 / 1e18;
    format!("{eth:.6}")
}

/// Format Wei as Gwei with 2 decimal places.
pub fn format_gwei(wei: u128) -> String {
    let gwei = wei as f64 / 1e9;
    format!("{gwei:.2}")
}
