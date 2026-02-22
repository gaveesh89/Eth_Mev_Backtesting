# AGENTS.md — MEV Backtest Toolkit

> Copilot, Cursor, and Claude: read this file before every task in this project.
> It defines commands, conventions, constraints, and canonical code style.
> Do NOT re-read it every turn — load it once per session and reference as needed.

---

## Project Identity

Educational Rust toolkit for replaying historical Ethereum blocks and understanding MEV mechanics.
**Analysis only — does NOT submit transactions to any relay, RPC, or network.**

Codebase language: Rust (stable, no nightly features)
Workspace: Cargo multi-crate workspace

---

## Commands (run these to verify work)

```bash
cargo check                          # Syntax + type check (run after every generation)
cargo check --package mev-data       # Check single crate
cargo clippy -- -D warnings          # Lint — fail on any warning
cargo fmt --all                      # Format all crates
cargo nextest run                    # Run all unit tests
cargo nextest run --package mev-sim  # Run tests in one crate
cargo doc --no-deps --document-private-items  # Verify docs build
cargo bench --package mev-sim        # Run benchmarks
```

**Verification gate (must pass before any commit):**
```
cargo check → cargo clippy -- -D warnings → cargo fmt --all → cargo nextest run
```

---

## Workspace Structure

```
mev-backtest-toolkit/
├── AGENTS.md                    ← you are here
├── spec.md                      ← full architecture reference (read for context)
├── Cargo.toml                   ← workspace root, pinned deps
├── crates/
│   ├── mev-data/                ← data ingestion (SQLite, Parquet, RPC)
│   ├── mev-sim/                 ← REVM simulation engine
│   ├── mev-analysis/            ← P&L computation, classification
│   └── mev-cli/                 ← binary entry point (clap)
├── data/                        ← SQLite snapshots (gitignored for >50MB files)
├── docs/                        ← tutorials, architecture diagrams
└── tests/                       ← workspace-level integration tests
```

---

## Canonical Code Patterns

### Struct + Constructor
```rust
pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// Creates or opens a SQLite database with WAL mode enabled.
    ///
    /// # Errors
    /// Returns error if the database cannot be opened or migrations fail.
    pub fn new(path: &str) -> eyre::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }
}
```

### Error Handling
```rust
// ✅ Always: propagate with ?  and eyre context
let block = store.get_block(number)
    .wrap_err_with(|| format!("failed to load block {number}"))?;

// ❌ Never: unwrap or expect in library crates
let block = store.get_block(number).unwrap(); // BANNED
```

### Async Functions
```rust
// ✅ Always: instrument public async functions
#[tracing::instrument(skip(self), fields(block = block_number))]
pub async fn fetch_block(&self, block_number: u64) -> eyre::Result<Block> { ... }
```

### Module Doc Comments
```rust
//! # Store Module
//!
//! SQLite storage layer for mempool transactions and on-chain block data.
//! Uses WAL mode for concurrent read performance and prepared statements
//! for batch insert throughput.
//!
//! ## Why SQLite?
//! rbuilder uses this same pattern for rapid local iteration without
//! requiring a running database server.
```

### Unit Test Structure
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        Store::new(":memory:").expect("in-memory store should always open")
    }

    #[test]
    fn insert_and_retrieve_block() {
        let store = test_store();
        let block = sample_block(18_000_000);
        store.insert_block(&block).unwrap();
        let retrieved = store.get_block(18_000_000).unwrap();
        assert_eq!(retrieved, Some(block));
    }
}
```

---

## Boundaries

### ✅ Always
- Use `eyre::Result` for all fallible functions in library crates
- Add `//!` module-level doc on every `lib.rs` and every module file
- Add `///` doc on every `pub` function, struct, and enum
- Use `tracing` macros (`info!`, `debug!`, `warn!`) — never `println!`
- Use `#[tracing::instrument]` on all public async functions
- Write at least one unit test per public function (happy path minimum)
- Run `cargo check` after every file generation

### ⚠️ Ask Before
- Adding new workspace-level dependencies to root `Cargo.toml`
- Changing existing SQLite schema (could break existing databases)
- Adding any `unsafe` block (requires written justification in doc comment)
- Changing existing public API signatures (breaking change)

### 🚫 Never
- `unwrap()` or `expect()` in library crates (`mev-data`, `mev-sim`, `mev-analysis`)
- `println!` anywhere (use `tracing` macros)
- `unsafe` without `# Safety` doc section explaining invariants
- Nightly Rust features (project must build on stable)
- Modifying pinned dependency versions in workspace `Cargo.toml`
- Deleting or weakening existing passing tests
- Adding transaction submission, relay calls, or mainnet-affecting code

---

## Ethereum Type Conventions

```rust
// ✅ Use alloy types everywhere
use alloy::primitives::{Address, U256, Bytes, B256};

// Store hex in SQLite as TEXT, parse on read
// Wei values as TEXT (U256 doesn't fit in SQLite INTEGER safely)
// Block numbers as INTEGER (u64 fits in SQLite INTEGER)
// Tx hashes as TEXT (lowercase hex with 0x prefix)
```

---

## Git Workflow

```bash
# After each successfully verified prompt:
git add -A
git commit -m "P-N: brief description of what was added"

# Feature branch per phase:
git checkout -b feat/phase-0-data-pipeline
git checkout -b feat/phase-1-sim-engine
git checkout -b feat/phase-2-analysis
```

---

## Performance Expectations (reference targets)

| Operation | Target | How to measure |
|-----------|--------|----------------|
| Single tx simulate (warm state) | < 5ms | `std::time::Instant` |
| EGP sort 100 txs | < 1ms | criterion bench |
| SQLite batch insert 1000 rows | < 100ms | criterion bench |
| Block range fetch 10 blocks | < 30s (public RPC) | wall clock |
