# MempoolParser Plan — Flashbots Mempool-Dumpster Parquet Integration

## Overview

Implement `MempoolParser` struct in `mev-data/src/mempool.rs` to:
1. Download daily Parquet files from Flashbots mempool-dumpster
2. Parse Parquet columns into `MempoolTransaction` structs  
3. Filter by block number range
4. Orchestrate download + parse + store workflow

---

## Expected Schema: Flashbots Mempool-Dumpster Parquet

**Source URL Pattern:**
```
https://mempool-dumpster.flashbots.net/ethereum/mainnet/{YYYY-MM}/transactions/{YYYY-MM-DD}.parquet
```

**Expected Columns in Parquet:**
| Column | Type | Maps To | Notes |
|--------|------|---------|-------|
| `hash` or `tx_hash` | Utf8/Binary | `MempoolTransaction.hash` | Lowercase hex with 0x |
| `block_number` | UInt64 or Int64 | `MempoolTransaction.block_number` | NULL if mempool-only |
| `timestamp_ms` | UInt64 or Int64 | `MempoolTransaction.timestamp_ms` | Unix ms |
| `from_address` or `from` | Utf8/Binary | `MempoolTransaction.from_address` | Lowercase 0x-prefixed |
| `to_address` or `to` | Utf8/Binary | `MempoolTransaction.to_address` | NULL for contract creation |
| `value` | Utf8/Binary or UInt256 | `MempoolTransaction.value` | Wei as string |
| `gas` or `gas_limit` | UInt64 or Int64 | `MempoolTransaction.gas_limit` | Gas limit |
| `gas_price` | Utf8/Binary or UInt256 | `MempoolTransaction.gas_price` | Wei as string (type 0) |
| `gas_tip` or `max_priority_*` | Utf8/Binary or UInt256 | `MempoolTransaction.max_priority_fee_per_gas` | Wei as string (type 2) |
| `gas_max_fee` or `max_fee_*` | Utf8/Binary or UInt256 | `MempoolTransaction.max_fee_per_gas` | Wei as string (type 2) |
| `nonce` | UInt64 | `MempoolTransaction.nonce` | Tx nonce |
| `input` or `input_data` | Utf8/Binary | `MempoolTransaction.input_data` | Hex-encoded calldata |
| `type` or `tx_type` | UInt8 or UInt32 | `MempoolTransaction.tx_type` | 0=legacy, 2=EIP1559 |
| `raw_tx` or `rlp` (optional) | Utf8/Binary | `MempoolTransaction.raw_tx` | Hex RLP; can compute if needed |

---

## MempoolParser Struct Design

```rust
pub struct MempoolParser {
    // HTTP client for downloads (persistent, connection pooling)
    client: reqwest::Client,
    
    // Temporary directory for downloaded Parquet files
    temp_dir: PathBuf,
    
    // Cache of parsed transactions (optional, for multi-day workflows)
    // cache: DashMap<String, Vec<MempoolTransaction>>,
}
```

---

## Method Signatures & Arrow/Parquet APIs

### 1. `new(temp_dir: Option<&str>) → Result<Self>`

**Purpose:** Initialize parser with HTTP client and temp storage.

**Implementation Path:**
- Use `reqwest::Client::builder()` with default pool settings
- Create or verify temp directory at `temp_dir` (default: `./data/temp/`)
- Return initialized `MempoolParser`

**No new crate APIs; standard setup.**

---

### 2. `download_day(&self, date: &str) → Result<PathBuf>` 
**Signature:** `&str` format: `"2025-02-22"`

**Purpose:** Download single day's Parquet file from Flashbots.

**Implementation Path:**
1. Parse `date` string to extract YYYY, MM, DD
2. Build URL: `https://mempool-dumpster.flashbots.net/ethereum/mainnet/{YYYY-MM}/transactions/{YYYY-MM-DD}.parquet`
3. Use `reqwest::Client::get(url)` → `response.bytes()` with timeout (30s)
4. Write to `temp_dir/{YYYY-MM-DD}.parquet`
5. Return local `PathBuf`

**Arrow/Parquet APIs Used:**
- None directly (pure HTTP download)

**Error Handling:**
- Network errors: wraps in `eyre::Result` with context (date, URL)
- File I/O: disk write failures
- HTTP 404: file not found for that date

---

### 3. `parse_parquet(&self, path: &Path) → Result<Vec<MempoolTransaction>>`

**Purpose:** Read Parquet file, extract rows, map columns to `MempoolTransaction` structs.

**Implementation Path:**

#### Step A: Open Parquet file
- Use `parquet::arrow::ArrowReader` or `parquet::arrow::ParquetRecordBatchReader`
- Or higher-level: `arrow::parquet::read::ParquetFileReader`
- API: `ParquetRecordBatchReader::try_new(File::open(path)?)?`

#### Step B: Read schema metadata
- Call `reader.schema()` to get `SchemaRef`
- Inspect column names to handle variations (hash vs tx_hash, etc.)
- Build column index map: `HashMap<&str, usize>` for flexible lookup

#### Step C: Stream record batches
- Iterate `reader.next()` → `Option<RecordBatch>`
- **Why streaming:** Parquet files can be large (100MB+), avoid loading all into memory

#### Step D: Extract and cast columns per row
- For each `RecordBatch`, iterate rows (or use batch operations if possible)
- Extract each column using `record_batch.column(col_index).as_any()` 
- Downcast to typed arrays:
  - `as_any().downcast_ref::<Utf8Array>()` for strings (hash, addresses, values)
  - `as_any().downcast_ref::<UInt64Array>()` for uint64 (block_number, timestamp_ms, nonce, gas_limit)
  - `as_any().downcast_ref::<UInt8Array>()` for tx_type
  - Handle NULL values with `.is_null(i)` check

#### Step E: Map to MempoolTransaction struct
```rust
// Pseudocode (not real code)
for batch in reader {
    let hash_col = batch.column(hash_idx).as_any().downcast_ref::<Utf8Array>()?;
    let block_col = batch.column(block_idx).as_any().downcast_ref::<UInt64Array>()?;
    // ... extract all 14 fields
    for row_idx in 0..batch.num_rows() {
        let tx = MempoolTransaction {
            hash: hash_col.value(row_idx).to_string(),
            block_number: if block_col.is_null(row_idx) { None } else { Some(block_col.value(row_idx)) },
            // ... etc for 12 more fields
        };
        txs.push(tx);
    }
}
```

**Arrow/Parquet APIs Used:**
- `parquet::arrow::ArrowReader` (trait)
- `parquet::arrow::ParquetRecordBatchReader` (concrete type)
- `arrow::array::RecordBatch` (row batch)
- `arrow::array::AsArray` (accessor trait for `as_any()`, `as_primitive()`)
- `arrow::array::StringArray` (Utf8 column wrapper)
- `arrow::array::UInt64Array`, `arrow::array::UInt8Array` (numeric columns)
- `arrow::datatypes::Schema` (metadata)

**Data Flow:**
```
File → ParquetRecordBatchReader → RecordBatch stream →
  Column extraction with downcast → Row-wise mapping →
  Vec<MempoolTransaction>
```

**Error Handling:**
- File not found or corrupted Parquet: I/O error
- Schema mismatch (missing expected columns): descriptive error
- Type downcast failure: schema validation error with column name
- All wrapped in `eyre::Result` with context

---

### 4. `filter_by_block_range(&self, txs: Vec<MempoolTransaction>, start: u64, end: u64) → Vec<MempoolTransaction>`

**Purpose:** Retain only txs with `block_number` in range `start..=end`.

**Implementation Path:**
1. Filter predicate: `tx.block_number.map_or(false, |bn| bn >= start && bn <= end)`
2. Return filtered vec

**Arrow/Parquet APIs Used:**
- None (post-parse filtering in memory)

**Why separate method:** Allows filtering before batch insert to reduce DB load.

---

### 5. `download_and_store(&self, date: &str, store: &Store, block_range: Option<(u64, u64)>) → Result<usize>`

**Purpose:** Orchestrator combining download → parse → filter → insert into SQLite.

**Implementation Path:**
1. Call `self.download_day(date)?` → local `PathBuf`
2. Call `self.parse_parquet(&path)?` → `Vec<MempoolTransaction>`
3. If `block_range = Some((start, end))`, filter: `self.filter_by_block_range(txs, start, end)`
4. Call `store.insert_mempool_txs(&filtered_txs)?` → count
5. Return count of inserted rows
6. (Optional) Clean up temp file: `std::fs::remove_file(path)?`

**Error Handling:**
- Propagate all errors from sub-methods with context
- Use `.wrap_err_with()` to add semantic meaning

**Signature:**
```rust
pub async fn download_and_store(
    &self,
    date: &str,
    store: &Store,
    block_range: Option<(u64, u64)>,
) -> Result<usize>
```

**Why async?** HTTP download should not block; integrate with tokio runtime later.

**Instrumentation:**
```rust
#[tracing::instrument(skip(self, store), fields(date = date, block_range = ?block_range))]
pub async fn download_and_store(...) -> Result<usize> { ... }
```

---

## Dependencies

All already in `Cargo.toml` (pinned in workspace):
- `reqwest = "0.12"` — HTTP client with async support
- `parquet = "53"` — Parquet file format reader
- `arrow = "53"` — Arrow in-memory columnar format + array types
- `eyre = "0.6"` — Error context chains
- `tracing = "0.1"` — Structured logging

**No new dependencies required.**

---

## Module Structure

```rust
// crates/mev-data/src/mempool.rs

//! Flashbots mempool-dumpster Parquet ingestion.
//!
//! Downloads daily transaction snapshots, parses columnar data into
//! MempoolTransaction structs, and optionally filters by block range
//! before storing in SQLite.

use eyre::Result;
use std::path::{Path, PathBuf};
use crate::types::MempoolTransaction;

pub struct MempoolParser { ... }

impl MempoolParser {
    pub fn new(temp_dir: Option<&str>) -> Result<Self> { ... }
    pub fn download_day(&self, date: &str) -> Result<PathBuf> { ... }
    pub fn parse_parquet(&self, path: &Path) -> Result<Vec<MempoolTransaction>> { ... }
    pub fn filter_by_block_range(&self, txs: Vec<MempoolTransaction>, start: u64, end: u64) -> Vec<MempoolTransaction> { ... }
    pub async fn download_and_store(&self, date: &str, store: &Store, block_range: Option<(u64, u64)>) -> Result<usize> { ... }
}
```

---

## Testing Strategy

### Unit Tests (in `mempool.rs`)

1. **`test_parse_parquet_valid`** — Download sample Parquet (or mock), verify all 14 fields map correctly
2. **`test_parse_parquet_missing_field`** — Parquet file missing a required column → descriptive error
3. **`test_filter_by_block_range`** — Insert mixed block numbers, verify range filter keeps only in-range

### Integration Tests (future, Phase 1)

1. **`test_download_and_store_e2e`** — Real download from Flashbots for a past date, verify insertion count

---

## Error Scenarios Handled

| Scenario | Error Type | eyre Context |
|----------|-----------|--------------|
| Network timeout | reqwest error | "Failed to download {date}: timeout after 30s" |
| HTTP 404 | HTTP status | "Mempool file not found for {date}" |
| Parquet corruption | parquet crate error | "Invalid Parquet file: {path}" |
| Schema mismatch | type downcast error | "Parquet column {name} not found or wrong type" |
| File I/O | std::io::Error | "Failed to read {path}" |
| Empty result | (not error) | Return empty vec; let caller decide |

---

## Performance Considerations

- **Streaming:** Use `ParquetRecordBatchReader` iterator to avoid loading entire file
- **Batch size:** Default ~8192 rows per record batch (Arrow standard)
- **Network:** Reqwest pooling minimizes connection overhead for multi-day downloads
- **Memory:** Vec accumulation of MempoolTransaction; acceptable for 100k-1M txs (typical daily volume ~1-2M txs, but most have NULL block_number; filtered subset ~50k-200k)
- **SQLite batch:** Insert methods already use transactions; no changes needed

---

## Next Steps After Implementation

1. Create integration test with real (or fixture) Parquet file
2. Add CLI command: `mev-cli mempool download-day --date 2025-02-22 --block-range 21000000..21000100`
3. Extend to multi-day range: `download_and_store_range(start_date, end_date, store, block_range)`
4. Add progress bar using `indicatif` crate (already in Cargo.toml)

---
