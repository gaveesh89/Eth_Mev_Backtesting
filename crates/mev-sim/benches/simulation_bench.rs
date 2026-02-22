//! Benchmarks for mev-sim core components.
//!
//! Uses pre-seeded in-memory state (no real RPC) for reproducible performance testing.
//! Run with: `cargo bench --package mev-sim`

use alloy::primitives::address;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mev_analysis::pnl::format_eth;
use mev_data::types::MempoolTransaction;
use mev_sim::ordering::{apply_nonce_constraints, order_by_egp};
use mev_sim::strategies::arbitrage::{detect_v2_arb_opportunity, PoolState};

/// Generates a sample MempoolTransaction with given hash and gas price.
fn sample_tx(hash_suffix: u64, gas_price: u128) -> MempoolTransaction {
    MempoolTransaction {
        hash: format!("0x{:064x}", hash_suffix),
        block_number: None,
        timestamp_ms: 1708617600000,
        from_address: format!("0x{:040x}", hash_suffix % 100),
        to_address: Some("0x70997970c51812e339d9b73b0245ad59e15ebbf9".to_string()),
        value: "0x0".to_string(),
        gas_limit: 21000,
        gas_price: format!("0x{:x}", gas_price),
        max_fee_per_gas: "0x0".to_string(),
        max_priority_fee_per_gas: "0x0".to_string(),
        nonce: hash_suffix / 100,
        input_data: "0x".to_string(),
        tx_type: 0,
        raw_tx: "0xf86a8085012a05f20082520894d8da6bf26964af9d7eed9e03e53415d37aa96045880de0b6b3a764000080".to_string(),
    }
}

/// Generates a mock PoolState at given reserves.
fn sample_pool(address_suffix: u8, reserve0: u128, reserve1: u128) -> PoolState {
    PoolState {
        address: address(&format!("0x{:040x}", address_suffix)),
        token0: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        token1: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        reserve0,
        reserve1,
        fee_bps: 30,
    }
}

/// Helper to parse address from string.
fn address(s: &str) -> alloy::primitives::Address {
    s.parse()
        .unwrap_or_else(|_| alloy::primitives::address!("0000000000000000000000000000000000000000"))
}

/// Benchmark: Order 100 transactions by effective gas price.
///
/// Creates 100 transactions with varying gas prices and measures time to sort them.
/// Time should be < 1ms on modern hardware.
fn bench_egp_sort_100_txs(c: &mut Criterion) {
    c.bench_function("egp_sort_100_txs", |b| {
        b.iter_batched(
            || {
                // Generate 100 txs with gas prices 1-100 gwei (shuffled)
                (0..100)
                    .map(|i| {
                        let gas_price = ((i * 37 + 13) % 100 + 1) as u128 * 1_000_000_000;
                        sample_tx(i as u64, gas_price)
                    })
                    .collect::<Vec<_>>()
            },
            |txs| {
                let base_fee = 1_000_000_000u128; // 1 gwei
                order_by_egp(black_box(txs), black_box(base_fee))
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark: Apply nonce constraints to 50 transactions from 10 senders.
///
/// Tests the performance of filtering out transactions with nonce gaps.
/// Time should be < 100µs on modern hardware.
fn bench_nonce_constraints_50_txs(c: &mut Criterion) {
    c.bench_function("nonce_constraints_50_txs", |b| {
        b.iter_batched(
            || {
                // 50 txs from 10 senders with varying nonces
                (0..50)
                    .map(|i| {
                        let sender = i % 10;
                        let nonce = i / 10;
                        let mut tx = sample_tx(i as u64, 20_000_000_000);
                        tx.from_address = format!("0x{:040x}", sender);
                        tx.nonce = nonce as u64;
                        tx
                    })
                    .collect::<Vec<_>>()
            },
            |txs| apply_nonce_constraints(black_box(txs)),
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark: Detect arbitrage opportunities across 10 pool pairs.
///
/// Evaluates 10 pairs of pools with varying price discrepancies.
/// Tests the arb detection algorithm without REVM simulation.
/// Time should be < 500µs on modern hardware.
fn bench_arb_detection_10_pools(c: &mut Criterion) {
    c.bench_function("arb_detection_10_pools", |b| {
        b.iter_batched(
            || {
                // 10 pool pairs with varying discrepancies
                (0..10)
                    .map(|i| {
                        let price_multiplier = 1000 + (i as u128 * 10); // Create slight discrepancy
                        if i % 2 == 0 {
                            sample_pool(i as u8, 1_000_000, 2_000_000_000)
                        } else {
                            sample_pool(i as u8, 1_000_000, price_multiplier)
                        }
                    })
                    .collect::<Vec<_>>()
            },
            |pools| {
                // Evaluate all pairs for arb opportunities
                let base_fee = 0u128;
                for i in 0..pools.len() - 1 {
                    for j in (i + 1)..pools.len() {
                        let _ = detect_v2_arb_opportunity(
                            black_box(&pools[i]),
                            black_box(&pools[j]),
                            black_box(base_fee),
                        );
                    }
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Benchmark: Format Wei to ETH string 10,000 times.
///
/// Tests the performance of the format_eth function with various Wei values.
/// Time should be < 5ms on modern hardware.
fn bench_eth_format(c: &mut Criterion) {
    c.bench_function("eth_format_10k_calls", |b| {
        b.iter(|| {
            // Format 10,000 different Wei values
            for i in 0..10_000u128 {
                let wei = (i * 1_234_567_890) % (1_000_000_000_000_000_000_000);
                format_eth(black_box(wei));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_egp_sort_100_txs,
    bench_nonce_constraints_50_txs,
    bench_arb_detection_10_pools,
    bench_eth_format
);
criterion_main!(benches);
