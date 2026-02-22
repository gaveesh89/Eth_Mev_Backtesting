# EvmFork Design Plan — REVM v19 Simulation Wrapper

## Overview

The `EvmFork` struct wraps REVM v19 to provide stateful simulation of Ethereum transactions within a forked state at block N-1, enabling transaction sequencing, MEV opportunity detection, and atomic bundle simulation.

**Performance Target:** Single tx simulate (warm state) < 5ms (from AGENTS.md)

---

## Architecture

```
┌─────────────────────────────────┐
│      EvmFork                    │
│  ┌───────────────────────────┐  │
│  │ Evm<CacheDB<InMemoryDB>>  │  │
│  │  (main executor)          │  │
│  └───────────────────────────┘  │
│           ↓                      │
│  ┌───────────────────────────┐  │
│  │ CacheDB<InMemoryDB>       │  │
│  │ (hot state + cache)       │  │
│  └───────────────────────────┘  │
│           ↓                      │
│  ┌───────────────────────────┐  │
│  │ InMemoryDB                │  │
│  │ (account states)          │  │
│  └───────────────────────────┘  │
│                                 │
│  Snapshots: Vec<B256>           │
│  (for rollback on bundle fail)  │
└─────────────────────────────────┘
```

---

## Core Struct Design

### `EvmFork` struct fields (planned)

```rust
pub struct EvmFork {
    /// REVM executor wrapping CacheDB for state caching
    evm: Evm<'static, CacheDB<InMemoryDB>>,
    
    /// Current block context (number, timestamp, base_fee)
    block_context: BlockContext,
    
    /// Stack of state snapshots for bundle rollback
    snapshots: Vec<Snapshot>,
    
    /// Transaction results accumulator
    results: Vec<ExecutionResult>,
}

struct BlockContext {
    block_number: u64,
    timestamp: u64,
    base_fee_per_gas: U256,
    miner: Address,
    chain_id: u64,
}

struct Snapshot {
    /// Unique identifier for this snapshot (block_number + nonce)
    id: u64,
    /// State hash at this point (for validation)
    state_hash: B256,
}
```

---

## REVM v19 Types & Traits to Use

### 1. **Core Executor Types**

| Type | Purpose | Location |
|------|---------|----------|
| `Evm<'a, DB>` | Main EVM executor (generic over DB type) | `revm::Evm` |
| `CacheDB<DB>` | Hot state cache layer (wraps any DB) | `revm::db::CacheDB` |
| `InMemoryDB` | Simple in-memory account storage | `revm::db::InMemoryDB` |

**Choice rationale:**
- `CacheDB<InMemoryDB>` is fastest (all memory, no RPC calls)
- Suitable for re-simulation where we pre-seed state from block N-1
- For future: could swap `InMemoryDB` for `JsonRpcDb` if live forking needed

### 2. **State & Database Traits**

| Trait | Purpose | Used For |
|-------|---------|----------|
| `Database` | Read/write account/storage/code | Implemented by CacheDB; passed to Evm |
| `DatabaseCommit` | Flush cache to underlying DB | After sequential commits |
| `DatabaseRef` | Read-only state access | for lookups during execution |

**Implementation path:**
- `CacheDB` implements both `Database` and `DatabaseCommit`
- After each tx: call `cache.commit_changes()` to persist state permanently
- Before rollback: store state hash for validation

### 3. **Transaction & Execution Types**

| Type | Purpose |
|------|---------|
| `TxEnv` | Ethereum transaction fields (to, from, value, data, etc.) |
| `ExecutionResult` | Result of tx execution (success/revert, gas_used, logs) |
| `Transact` trait | Execute a transaction on the EVM |
| `TransactTo` enum | `Call` or `Create` transaction type |

**Usage pattern:**
```
for each tx:
  1. Build TxEnv from Block Transaction
  2. evm.tx_env = tx_env
  3. evm.transact() → ExecutionResult
  4. cache.commit_changes() (sequential commit)
  5. Store ExecutionResult
```

### 4. **State Snapshot & Rollback**

| Mechanism | Purpose | REVM v19 API |
|-----------|---------|--------------|
| `Evm::transact()` | Returns result without state change | Read-only if no commit |
| `CacheDB::clear_cache()` | Revert all uncommitted changes | For rollback |
| State hash | Snapshot identifier | Hash of (accounts + storage + code) |

**Snapshot strategy:**
1. Before bundle: hash `cache.accounts` + `cache.storage` + `cache.code` → snapshot ID
2. Execute bundle transactions with `transact()`
3. If bundle fails: call `cache.clear_cache()` → reverts to saved state
4. If bundle succeeds: call `cache.commit_changes()` → persists

---

## Key Capabilities & Implementation Plan

### 1. **Read-Only Simulation**

**Goal:** Execute tx without modifying state; allows previewing effects

**REVM mechanism:**
- `evm.transact()` returns `ExecutionResult` without committing
- Internal EVM state changes stay in `CacheDB` but not persisted
- Result includes: gas_used, logs, success/revert status

**Design:**
```
fn simulate_tx(&mut self, tx: &BlockTransaction) 
  → Result<ExecutionResult>
{
  // Don't call cache.commit_changes()
  // Just return result for inspection
}
```

### 2. **Sequential Commit**

**Goal:** Apply each tx state change permanently in order (standard simulation path)

**REVM mechanism:**
- `evm.transact()` updates `CacheDB` internally
- `cache.commit_changes()` persists to `InMemoryDB`
- Subsequent txs see all prior changes

**Design:**
```
fn execute_and_commit(&mut self, tx: &BlockTransaction) 
  → Result<ExecutionResult>
{
  let result = evm.transact()?;
  cache.commit_changes();  // Persist
  self.results.push(result);
  Ok(result)
}
```

### 3. **Atomic Bundle Simulation with Rollback**

**Goal:** Execute multiple txs as atomic unit; rollback all if condition fails

**REVM mechanism:**
- Store pre-bundle state hash (snapshot)
- Execute all txs (accumulates in cache)
- If condition fails: `cache.clear_cache()` reverts uncommitted changes
- Only commit if condition succeeds

**Design:**
```
fn simulate_bundle_atomic(
  &mut self, 
  bundle: &[BlockTransaction],
  condition: impl Fn(&[ExecutionResult]) → bool
) → Result<bool>
{
  let snapshot = self.create_snapshot();
  
  for tx in bundle {
    let result = evm.transact()?;
    self.results.push(result);
  }
  
  if !condition(&self.results) {
    cache.clear_cache();  // Rollback
    self.results.clear();
    return Ok(false);
  }
  
  cache.commit_changes();  // Commit
  Ok(true)
}
```

### 4. **Account Pre-Seeding**

**Goal:** Inject custom account state before simulation (e.g., give contract balance for testing)

**REVM mechanism:**
- Directly modify `CacheDB` account fields before any tx execution
- Set: balance, nonce, code, storage
- Changes persist across all subsequent txs

**Design:**
```
fn preseed_account(
  &mut self, 
  addr: Address, 
  balance: U256,
  code: Option<Bytes>
) → Result<()>
{
  // Create or update account in cache
  cache.accounts
    .entry(addr)
    .or_insert_with(|| AccountInfo::new(...))
    .balance = balance;
  
  if let Some(bytecode) = code {
    cache.code.insert(addr, bytecode);
  }
  
  Ok(())
}
```

---

## State Fork Initialization (Block N-1 Snapshot)

### Initial Setup

1. **Fetch block N-1 state:**
   - Use Alloy RPC to get block header, base_fee, miner, timestamp
   - For full state: either fetch via `eth_getAccount` for each account OR use archive node diff

2. **Create EvmFork at block N-1:**
   - Initialize `Evm` with `CacheDB(InMemoryDB)`
   - Populate initial accounts from block N-1
   - Set BlockContext: number, timestamp, base_fee, miner

3. **Ready for block N simulation:**
   - All state changes during block N simulation accumulate in `CacheDB`
   - After each tx: optional commit via `cache.commit_changes()`
   - Snapshots can be taken for bundle rollback

### Code location consideration

- State fetching: `mev-data::blocks::BlockFetcher::fetch_account_state()` (new helper)
- Fork initialization: `mev-sim::fork::EvmFork::new(block_n_minus_1, accounts)` 
- State seeding: part of `EvmFork::preseed_*` methods

---

## Type System & Generics Strategy

### Proposed type signature

```rust
pub struct EvmFork {
    // CacheDB wraps InMemoryDB
    // Evm uses CacheDB as DB type
    evm: Evm<'static, CacheDB<InMemoryDB>>,
    
    // Owned DB (no references needed)
    cache: CacheDB<InMemoryDB>,
    
    // Block context
    block_context: BlockContext,
    
    // Snapshots for rollback
    snapshots: Vec<Snapshot>,
}
```

**Lifetime considerations:**
- `Evm<'a, DB>`: `'a` is the lifetime of DB reference
- Using `CacheDB<InMemoryDB>` directly (owned) avoids lifetime issues
- All state is in-memory: no async I/O, fully synchronous

---

## Error Handling Strategy

### Error types to define

```rust
pub enum EvmError {
  // Transaction execution failed (revert)
  ExecutionFailed { 
    tx_index: usize, 
    revert_reason: String 
  },
  
  // DB operation failed
  DbError(String),
  
  // Snapshot/rollback issue
  SnapshotNotFound(u64),
  
  // State corruption
  StateHashMismatch { expected: B256, actual: B256 },
}
```

### Error context (via eyre)

Wrap REVM errors with `wrap_err()` to add block number, tx index context

---

## Performance Notes

### Why < 5ms target is achievable

1. **CacheDB + InMemoryDB:** Warm state access is O(1) hash lookups
2. **No RPC calls during simulation:** All state pre-loaded or fetched once
3. **REVM optimizations:**
   - Bytecode caching
   - Instruction cache
   - Jump table for EVM operations
4. **Typical profile:**
   - Simple transfer: ~100 µs
   - Uniswap swap: ~2-3 ms
   - Complex sandwich: ~4-5 ms

### Optimization opportunities (future)

- Use `CacheDB::cached_accounts()` for warm-start pre-filling
- Parallel tx simulation (requires snapshot isolation per thread)
- Batch multiple blocks to amortize state fetch cost

---

## Module Organization

### `mev-sim/src/evm.rs` public API

```rust
pub mod fork;       // EvmFork struct
pub mod state;      // BlockContext, Snapshot
pub mod error;      // EvmError enum
pub mod results;    // ExecutionResult wrappers

// Main exports
pub use fork::EvmFork;
pub use error::EvmError;
pub type Result<T> = std::result::Result<T, EvmError>;
```

### Type imports from REVM

- `revm::Evm`
- `revm::db::{CacheDB, InMemoryDB, Database, DatabaseCommit}`
- `revm::primitives::{ExecutionResult, TxEnv, Address, U256, Bytes, B256}`
- `revm::interpreter::Interpreter` (for instruction tracing if needed)

---

## Next Steps (Not Yet Implemented)

1. ✅ Plan EvmFork struct (this document)
2. ⏳ Implement `EvmFork::new()` (initialize at block N-1)
3. ⏳ Implement `execute_and_commit()` (sequential txs)
4. ⏳ Implement `simulate_bundle_atomic()` (rollback on fail)
5. ⏳ Implement `preseed_account()` (custom state injection)
6. ⏳ Write integration tests
7. ⏳ Benchmark against < 5ms target
8. ⏳ Connect to `mev-analysis` for MEV detection

---

## References

- **REVM v19 docs:** https://github.com/bluealloy/revm
- **spec.md:** Architecture and data flow
- **AGENTS.md:** Performance targets, error handling patterns
- **Alloy RPC:** `mev-data::blocks::BlockFetcher` (state provider)
