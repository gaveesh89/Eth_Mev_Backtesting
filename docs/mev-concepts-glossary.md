# MEV Concepts Glossary

Comprehensive definitions of key MEV and Ethereum concepts. Each entry includes a concise definition (2–3 sentences) and relevant source links.

---

## MEV

**Maximal Extractable Value**: The total profit available to block builders, validators, and other participants through reordering, inserting, or censoring transactions in a block. MEV exists because transaction execution order affects state changes and can create arbitrage or liquidation opportunities. See: [Flashbots MEV Definition](https://docs.flashbots.net/), [MEV-Inspect](https://github.com/flashbots/mev-inspect-rs)

---

## Sandwich Attack

A frontrunning + backrunning MEV strategy where an attacker sees a pending victim transaction in the mempool, places their own transaction before it (frontrun), then places another transaction after it (backrun) to extract profit from the price movement. The victim's transaction executes in the middle ("sandwiched") and typically suffers losses. See: [MEV-Inspect Sandwich Detection](https://github.com/flashbots/mev-inspect-rs/tree/main/crates/mev-inspector/src/inspectors), [crates/mev-analysis/src/classify.rs](../crates/mev-analysis/src/classify.rs)

---

## Arbitrage

A MEV opportunity where two pools or markets offer different prices for the same asset. An arbitrageur buys at the lower price and sells at the higher price in a single atomic block, capturing the spread as profit. Uniswap V2 two-leg swaps (token0 → token1 → token0) are the classic example. See: [crates/mev-sim/src/strategies/arbitrage.rs](../crates/mev-sim/src/strategies/arbitrage.rs), [Uniswap V2 Model](https://docs.uniswap.org/contracts/v2/concepts/protocol-overview/how-uniswap-works)

---

## Backrun

The second leg of a frontrunning strategy: a transaction placed *after* a victim's transaction to benefit from the state changes it caused. Often paired with a frontrun as part of a MEV extraction strategy (e.g., sandwich attack). See: [Flashbots Docs](https://docs.flashbots.net/), [crates/mev-analysis/src/classify.rs](../crates/mev-analysis/src/classify.rs)

---

## Bundle

An atomic sequence of transactions that either all succeed or all revert as a unit. Bundles are used for MEV extraction (e.g., "frontrun tx + victim tx + backrun tx") and ensure that if the sandwich fails, no partial profit is captured. Flashbots Bundles popularized this concept. See: [Flashbots Bundles Spec](https://docs.flashbots.net/flashbots-auction/bundles), [spec.md § Data Flow](../spec.md)

---

## Searcher

An entity (bot, trader, protocol) that identifies and submits MEV opportunities (bundles) to block builders or directly to the network. Searchers use private order flow (MEV-Boost), pools, or mempool scanning to find profitable orderings. See: [Flashbots Searcher Guide](https://docs.flashbots.net/flashbots-auction/searchers), [MEV-Inspect Classifiers](https://github.com/flashbots/mev-inspect-rs)

---

## Builder

An entity that constructs blocks for validators by selecting a set of transactions and MEV bundles, ordered to maximize value for the validator. Flashbots and MEV-Boost popularized the "PBS builder" concept. Builders collect searcher bundles and mempool transactions, simulate orderings, and submit blocks to validators. See: [MEV-Boost Specification](https://github.com/flashbots/mev-boost), [rbuilder GitHub](https://github.com/flashbots/rbuilder)

---

## Proposer

An Ethereum validator that proposes (creates) a block for the chain and collects builder rewards. In MEV-Boost, the proposer accepts a header + payload from a builder without seeing the full contents. Post-merge, all validators are proposers. See: [Ethereum Consensus Spec](https://github.com/ethereum/consensus-specs), [MEV-Boost Spec](https://github.com/flashbots/mev-boost)

---

## MEV-Boost

Flashbots' open-source middleware that separates block building from proposing: proposers auction block space to competing builders, who submit sealed bids for the best available MEV. This reduces validator infrastructure costs and creates a competitive MEV market. See: [MEV-Boost GitHub](https://github.com/flashbots/mev-boost), [Flashbots Spec](https://docs.flashbots.net/mev-boost/introduction)

---

## Mempool

The Ethereum network's transaction waiting pool before inclusion in a block. Transactions in the mempool are visible to all nodes and MEV searchers, creating frontrunning opportunities. A "dark pool" or private mempool (e.g., MEV-Boost private block) hides transactions from public view. See: [Ethereum Docs](https://ethereum.org/en/developers/docs/blocks/#block-anatomy), [Flashbots dark pool](https://docs.flashbots.net/flashbots-auction/blocks)

---

## Private Order Flow

Transactions submitted directly to a builder, searcher, or MEV relay (not broadcast to the public mempool). Private order flow preserves transaction privacy and prevents frontrunning by competitors. Flashbots Relay, MEV-Boost, and Threshold Encryption protocols provide private order flow. See: [Flashbots Private Transactions](https://docs.flashbots.net/flashbots-auction/private-transactions), [TEE-based Solutions](https://github.com/flashbots/tee-builder)

---

## Base Fee

Ethereum's dynamic fee burned per unit of gas, determined by block utilization (EIP-1559). The base fee increases if blocks are >50% full and decreases if blocks are <50% full. All transactions must pay at least the base fee for inclusion; tips are separate. See: [EIP-1559 Spec](https://eips.ethereum.org/EIPS/eip-1559), [Ethereum Gas Docs](https://ethereum.org/en/developers/docs/gas/)

---

## Priority Fee

The tip paid to validators/proposers per unit of gas (EIP-1559, aka "tip"). Searchers and users set `maxPriorityFeePerGas` to bid for priority in a block; higher tips = faster inclusion. Priority fees are *not* burned; they go to the block proposer. See: [EIP-1559 Spec](https://eips.ethereum.org/EIPS/eip-1559), [crates/mev-analysis/src/pnl.rs](../crates/mev-analysis/src/pnl.rs)

---

## Effective Gas Price

The actual gas price paid by a transaction: for EIP-1559 transactions (type 2), it is `min(maxFeePerGas, baseFee + maxPriorityFeePerGas)`. For legacy transactions (type 0), it is always `gasPrice`. This is the key metric for transaction ordering simulation. See: [EIP-1559 Spec](https://eips.ethereum.org/EIPS/eip-1559), [crates/mev-sim/src/ordering.rs](../crates/mev-sim/src/ordering.rs)

---

## REVM

Ethereum Virtual Machine (EVM) library written in Rust for fast, lightweight execution without networking. REVM is used by block builders (rbuilder, Flashbots MEV-Inspect) for simulation and backtest txs. It supports all hardforks and is compatible with Alloy. See: [REVM GitHub](https://github.com/bluealloy/revm), [crates/mev-sim/src/evm.rs](../crates/mev-sim/src/evm.rs)

---

## Additional Resources

- **Specification**: [spec.md](../spec.md)
- **Flashbots Documentation**: https://docs.flashbots.net
- **MEV-Inspect Repository**: https://github.com/flashbots/mev-inspect-rs
- **Ethereum Docs**: https://ethereum.org/en/developers/
- **REVM**: https://github.com/bluealloy/revm
- **EIP-1559**: https://eips.ethereum.org/EIPS/eip-1559

