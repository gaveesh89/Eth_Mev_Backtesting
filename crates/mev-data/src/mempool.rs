//! Flashbots mempool-dumpster Parquet ingestion.
//!
//! Downloads daily transaction snapshots from Flashbots' public archive,
//! caches locally, and provides parsing into MempoolTransaction structs.

use chrono::NaiveDate;
use eyre::{Context, Result};
use futures::StreamExt;
use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::store::Store;
use crate::types::MempoolTransaction;

/// Download a single day's mempool-dumpster Parquet file from Flashbots.
///
/// Skips download if the file already exists locally. Shows progress bar
/// for bytes downloaded.
///
/// # Arguments
/// * `date` - Date in NaiveDate format (e.g., 2025-02-22)
/// * `data_dir` - Directory where .parquet file will be saved
///
/// # Returns
/// Path to the downloaded/existing .parquet file
///
/// # Errors
/// Returns error if network request fails, file I/O fails, or date formatting fails.
///
/// # Example
/// ```no_run
/// use chrono::NaiveDate;
/// use std::path::Path;
/// # use mev_data::mempool::download_day;
/// # async fn example() -> eyre::Result<()> {
/// let date = NaiveDate::from_ymd_opt(2025, 2, 22).unwrap();
/// let path = download_day(date, Path::new("./data")).await?;
/// println!("Downloaded to: {:?}", path);
/// # Ok(())
/// # }
/// ```
#[tracing::instrument(skip_all, fields(date = %date))]
pub async fn download_day(date: NaiveDate, data_dir: &Path) -> Result<PathBuf> {
    // Ensure data directory exists
    tokio::fs::create_dir_all(data_dir)
        .await
        .wrap_err_with(|| format!("failed to create data directory: {}", data_dir.display()))?;

    // Build local file path: data_dir/{YYYY-MM-DD}.parquet
    let filename = format!("{}.parquet", date.format("%Y-%m-%d"));
    let local_path = data_dir.join(&filename);

    // Skip download if file already exists
    if local_path.exists() {
        tracing::debug!(
            path = %local_path.display(),
            "parquet file already exists, skipping download"
        );
        return Ok(local_path);
    }

    // Build URL: https://mempool-dumpster.flashbots.net/ethereum/mainnet/{YYYY-MM}/transactions/{YYYY-MM-DD}.parquet
    let year_month = date.format("%Y-%m").to_string();
    let date_str = date.format("%Y-%m-%d").to_string();
    let url = format!(
        "https://mempool-dumpster.flashbots.net/ethereum/mainnet/{}/transactions/{}.parquet",
        year_month, date_str
    );

    tracing::info!(url = %url, "downloading mempool-dumpster parquet");

    // Create async HTTP client
    let client = reqwest::Client::new();

    // Send GET request with timeout
    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(300)) // 5 minute timeout for large files
        .send()
        .await
        .wrap_err_with(|| format!("failed to download from {}", url))?;

    // Get total file size for progress bar
    let total_size = response
        .content_length()
        .ok_or_else(|| eyre::eyre!("flashbots response missing content-length header"))?;

    // Initialize progress bar
    let progress_bar = ProgressBar::new(total_size);
    progress_bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .wrap_err("invalid progress bar template")?,
    );

    // Stream response body and write to file
    let mut file = tokio::fs::File::create(&local_path)
        .await
        .wrap_err_with(|| format!("failed to create file: {}", local_path.display()))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("failed to read response chunk")?;
        progress_bar.inc(chunk.len() as u64);

        file.write_all(&chunk)
            .await
            .wrap_err("failed to write to parquet file")?;
    }

    progress_bar.finish_with_message("download complete");

    tracing::info!(
        path = %local_path.display(),
        bytes = total_size,
        "mempool-dumpster parquet downloaded"
    );

    Ok(local_path)
}

/// Parse Parquet file into a vector of MempoolTransaction structs.
///
/// Streams record batches and maps columns to transaction fields.
/// Handles NULL values in optional fields (to_address, max_fee_per_gas, etc).
///
/// # Errors
/// Returns error if file cannot be opened, Parquet format is invalid,
/// or schema is missing expected columns.
pub fn parse_parquet(path: &Path) -> Result<Vec<MempoolTransaction>> {
    use arrow::array::{Array, StringArray, UInt32Array, UInt64Array};
    use arrow::record_batch::RecordBatchReader;
    use eyre::ContextCompat;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open parquet file: {}", path.display()))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .wrap_err("failed to parse parquet metadata")?;

    let reader = builder
        .build()
        .wrap_err("failed to build parquet record batch reader")?;

    let schema = reader.schema();

    // Get column indices by name (handle potential variations)
    let col_idx = |name: &str| -> Result<usize> {
        schema
            .index_of(name)
            .context(format!("column {} not found in parquet schema", name))
    };

    let hash_idx = col_idx("hash")?;
    let block_number_idx = col_idx("block_number")?;
    let timestamp_ms_idx = col_idx("timestamp_ms")?;
    let from_address_idx = col_idx("from_address")?;
    let to_address_idx = col_idx("to_address")?;
    let value_idx = col_idx("value")?;
    let gas_limit_idx = col_idx("gas_limit")?;
    let gas_price_idx = col_idx("gas_price")?;
    let max_fee_per_gas_idx = col_idx("max_fee_per_gas")?;
    let max_priority_fee_per_gas_idx = col_idx("max_priority_fee_per_gas")?;
    let nonce_idx = col_idx("nonce")?;
    let input_data_idx = col_idx("input_data")?;
    let tx_type_idx = col_idx("tx_type")?;
    let raw_tx_idx = col_idx("raw_tx")?;

    let mut txs = Vec::new();

    // Stream record batches
    for batch_result in reader {
        let batch = batch_result.wrap_err("failed to read record batch")?;

        // Extract columns from batch
        let hash_col = batch
            .column(hash_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("hash column is not string type")?;

        let block_number_col = batch
            .column(block_number_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("block_number column is not u64 type")?;

        let timestamp_ms_col = batch
            .column(timestamp_ms_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("timestamp_ms column is not u64 type")?;

        let from_address_col = batch
            .column(from_address_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("from_address column is not string type")?;

        let to_address_col = batch
            .column(to_address_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("to_address column is not string type")?;

        let value_col = batch
            .column(value_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("value column is not string type")?;

        let gas_limit_col = batch
            .column(gas_limit_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("gas_limit column is not u64 type")?;

        let gas_price_col = batch
            .column(gas_price_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("gas_price column is not string type")?;

        let max_fee_per_gas_col = batch
            .column(max_fee_per_gas_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("max_fee_per_gas column is not string type")?;

        let max_priority_fee_per_gas_col = batch
            .column(max_priority_fee_per_gas_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("max_priority_fee_per_gas column is not string type")?;

        let nonce_col = batch
            .column(nonce_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("nonce column is not u64 type")?;

        let input_data_col = batch
            .column(input_data_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("input_data column is not string type")?;

        let tx_type_col = batch
            .column(tx_type_idx)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .context("tx_type column is not u32 type")?;

        let raw_tx_col = batch
            .column(raw_tx_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("raw_tx column is not string type")?;

        // Map rows in batch to MempoolTransaction structs
        for row_idx in 0..batch.num_rows() {
            let tx = MempoolTransaction {
                hash: hash_col.value(row_idx).to_string(),
                block_number: if block_number_col.is_null(row_idx) {
                    None
                } else {
                    Some(block_number_col.value(row_idx))
                },
                timestamp_ms: timestamp_ms_col.value(row_idx),
                from_address: from_address_col.value(row_idx).to_string(),
                to_address: if to_address_col.is_null(row_idx) {
                    None
                } else {
                    Some(to_address_col.value(row_idx).to_string())
                },
                value: value_col.value(row_idx).to_string(),
                gas_limit: gas_limit_col.value(row_idx),
                gas_price: gas_price_col.value(row_idx).to_string(),
                max_fee_per_gas: max_fee_per_gas_col.value(row_idx).to_string(),
                max_priority_fee_per_gas: max_priority_fee_per_gas_col.value(row_idx).to_string(),
                nonce: nonce_col.value(row_idx),
                input_data: input_data_col.value(row_idx).to_string(),
                tx_type: tx_type_col.value(row_idx),
                raw_tx: raw_tx_col.value(row_idx).to_string(),
            };
            txs.push(tx);
        }
    }

    tracing::info!(tx_count = txs.len(), "parsed parquet file");
    Ok(txs)
}

/// Filter mempool transactions to keep only those with block_number in range.
///
/// Retains transactions where `block_number` is `Some` and falls within
/// the inclusive range `start..=end`.
///
/// # Arguments
/// * `txs` - Vector of transactions to filter
/// * `start` - Start of inclusive block number range
/// * `end` - End of inclusive block number range
///
/// # Returns
/// Filtered vector containing only in-range transactions
pub fn filter_by_block_range(
    txs: Vec<MempoolTransaction>,
    start: u64,
    end: u64,
) -> Vec<MempoolTransaction> {
    txs.into_iter()
        .filter(|tx| tx.block_number.is_some_and(|bn| bn >= start && bn <= end))
        .collect()
}

/// Download, parse, and store a day's mempool transactions.
///
/// Orchestrates the full workflow: download Parquet file from Flashbots,
/// parse into MempoolTransaction structs, and insert into SQLite.
///
/// # Arguments
/// * `date` - Date in NaiveDate format (e.g., 2025-02-22)
/// * `store` - SQLite Store for persistence
/// * `data_dir` - Directory for cached Parquet files
///
/// # Returns
/// Count of transactions inserted into database
///
/// # Errors
/// Returns error if download fails, parsing fails, or database insert fails.
#[tracing::instrument(skip(store), fields(date = %date))]
pub async fn download_and_store(date: NaiveDate, store: &Store, data_dir: &Path) -> Result<usize> {
    // Download or retrieve cached Parquet file
    let path = download_day(date, data_dir).await?;

    // Parse Parquet into transaction structs
    let txs = parse_parquet(&path)?;
    tracing::debug!(tx_count = txs.len(), "parsed transactions from parquet");

    // Insert into database
    let inserted = store.insert_mempool_txs(&txs)?;
    tracing::info!(
        inserted_count = inserted,
        "inserted transactions into database"
    );

    Ok(inserted)
}

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, StringBuilder, UInt32Builder, UInt64Builder};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    #[tokio::test]
    async fn download_day_skips_existing() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let data_dir = temp_dir.path();
        let date = NaiveDate::from_ymd_opt(2025, 2, 22).unwrap();

        // Pre-create the file
        let expected_path = data_dir.join("2025-02-22.parquet");
        tokio::fs::write(&expected_path, b"fake parquet content")
            .await
            .expect("write fake file");

        // Call download_day; should return path without downloading
        let result = download_day(date, data_dir).await.expect("download_day");

        assert_eq!(result, expected_path);

        // Verify file still has original content (not re-downloaded)
        let content = tokio::fs::read(&result).await.expect("read file");
        assert_eq!(content, b"fake parquet content");
    }

    #[test]
    fn date_formats_correctly() {
        let date = NaiveDate::from_ymd_opt(2025, 2, 22).unwrap();
        assert_eq!(date.format("%Y-%m-%d").to_string(), "2025-02-22");
        assert_eq!(date.format("%Y-%m").to_string(), "2025-02");
    }

    #[test]
    fn parse_parquet_creates_transactions() {
        // Create temporary parquet file
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let parquet_path = temp_dir.path().join("test.parquet");

        // Build test data for 2 transactions
        let mut hash_builder = StringBuilder::new();
        let mut block_number_builder = UInt64Builder::new();
        let mut timestamp_ms_builder = UInt64Builder::new();
        let mut from_address_builder = StringBuilder::new();
        let mut to_address_builder = StringBuilder::new();
        let mut value_builder = StringBuilder::new();
        let mut gas_limit_builder = UInt64Builder::new();
        let mut gas_price_builder = StringBuilder::new();
        let mut max_fee_per_gas_builder = StringBuilder::new();
        let mut max_priority_fee_per_gas_builder = StringBuilder::new();
        let mut nonce_builder = UInt64Builder::new();
        let mut input_data_builder = StringBuilder::new();
        let mut tx_type_builder = UInt32Builder::new();
        let mut raw_tx_builder = StringBuilder::new();

        // Row 1: complete transaction with block_number and to_address
        hash_builder.append_value("0xaabbcc");
        block_number_builder.append_value(100);
        timestamp_ms_builder.append_value(1000);
        from_address_builder.append_value("0xfrom1");
        to_address_builder.append_value("0xto1");
        value_builder.append_value("1000000");
        gas_limit_builder.append_value(21000);
        gas_price_builder.append_value("20000000000");
        max_fee_per_gas_builder.append_value("0");
        max_priority_fee_per_gas_builder.append_value("0");
        nonce_builder.append_value(1);
        input_data_builder.append_value("0x");
        tx_type_builder.append_value(0);
        raw_tx_builder.append_value("0xdeadbeef");

        // Row 2: transaction with NULL block_number and to_address
        hash_builder.append_value("0xddeeff");
        block_number_builder.append_null();
        timestamp_ms_builder.append_value(1001);
        from_address_builder.append_value("0xfrom2");
        to_address_builder.append_null();
        value_builder.append_value("2000000");
        gas_limit_builder.append_value(50000);
        gas_price_builder.append_value("25000000000");
        max_fee_per_gas_builder.append_value("80000000000");
        max_priority_fee_per_gas_builder.append_value("2000000000");
        nonce_builder.append_value(2);
        input_data_builder.append_value("0xcafebabe");
        tx_type_builder.append_value(2);
        raw_tx_builder.append_value("0xcafebabe");

        // Create arrays
        let hash_array: ArrayRef = Arc::new(hash_builder.finish());
        let block_number_array: ArrayRef = Arc::new(block_number_builder.finish());
        let timestamp_ms_array: ArrayRef = Arc::new(timestamp_ms_builder.finish());
        let from_address_array: ArrayRef = Arc::new(from_address_builder.finish());
        let to_address_array: ArrayRef = Arc::new(to_address_builder.finish());
        let value_array: ArrayRef = Arc::new(value_builder.finish());
        let gas_limit_array: ArrayRef = Arc::new(gas_limit_builder.finish());
        let gas_price_array: ArrayRef = Arc::new(gas_price_builder.finish());
        let max_fee_per_gas_array: ArrayRef = Arc::new(max_fee_per_gas_builder.finish());
        let max_priority_fee_per_gas_array: ArrayRef =
            Arc::new(max_priority_fee_per_gas_builder.finish());
        let nonce_array: ArrayRef = Arc::new(nonce_builder.finish());
        let input_data_array: ArrayRef = Arc::new(input_data_builder.finish());
        let tx_type_array: ArrayRef = Arc::new(tx_type_builder.finish());
        let raw_tx_array: ArrayRef = Arc::new(raw_tx_builder.finish());

        // Create schema
        let schema = Arc::new(Schema::new(vec![
            Field::new("hash", DataType::Utf8, false),
            Field::new("block_number", DataType::UInt64, true),
            Field::new("timestamp_ms", DataType::UInt64, false),
            Field::new("from_address", DataType::Utf8, false),
            Field::new("to_address", DataType::Utf8, true),
            Field::new("value", DataType::Utf8, false),
            Field::new("gas_limit", DataType::UInt64, false),
            Field::new("gas_price", DataType::Utf8, false),
            Field::new("max_fee_per_gas", DataType::Utf8, false),
            Field::new("max_priority_fee_per_gas", DataType::Utf8, false),
            Field::new("nonce", DataType::UInt64, false),
            Field::new("input_data", DataType::Utf8, false),
            Field::new("tx_type", DataType::UInt32, false),
            Field::new("raw_tx", DataType::Utf8, false),
        ]));

        // Create record batch
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                hash_array,
                block_number_array,
                timestamp_ms_array,
                from_address_array,
                to_address_array,
                value_array,
                gas_limit_array,
                gas_price_array,
                max_fee_per_gas_array,
                max_priority_fee_per_gas_array,
                nonce_array,
                input_data_array,
                tx_type_array,
                raw_tx_array,
            ],
        )
        .expect("create record batch");

        // Write to parquet file
        let file = std::fs::File::create(&parquet_path).expect("create parquet file");
        let mut writer = ArrowWriter::try_new(file, schema, None).expect("create arrow writer");
        writer.write(&batch).expect("write batch");
        writer.close().expect("close writer");

        // Parse parquet file
        let txs = parse_parquet(&parquet_path).expect("parse parquet");

        // Assertions
        assert_eq!(txs.len(), 2, "should parse 2 transactions");

        // Check row 1
        assert_eq!(txs[0].hash, "0xaabbcc");
        assert_eq!(txs[0].block_number, Some(100));
        assert_eq!(txs[0].timestamp_ms, 1000);
        assert_eq!(txs[0].from_address, "0xfrom1");
        assert_eq!(txs[0].to_address, Some("0xto1".to_string()));
        assert_eq!(txs[0].value, "1000000");
        assert_eq!(txs[0].gas_limit, 21000);
        assert_eq!(txs[0].gas_price, "20000000000");
        assert_eq!(txs[0].nonce, 1);
        assert_eq!(txs[0].tx_type, 0);

        // Check row 2 (NULL fields)
        assert_eq!(txs[1].hash, "0xddeeff");
        assert_eq!(txs[1].block_number, None, "block_number should be None");
        assert_eq!(txs[1].to_address, None, "to_address should be None");
        assert_eq!(txs[1].timestamp_ms, 1001);
        assert_eq!(txs[1].tx_type, 2);
        assert_eq!(txs[1].max_fee_per_gas, "80000000000");
        assert_eq!(
            txs[1].max_priority_fee_per_gas, "2000000000",
            "EIP1559 fees should be parsed"
        );
    }

    #[test]
    fn filter_by_block_range_keeps_only_in_range() {
        let txs = vec![
            // Tx 0: block 99 (before range)
            MempoolTransaction {
                hash: "0x01".to_string(),
                block_number: Some(99),
                timestamp_ms: 1000,
                from_address: "0xfrom".to_string(),
                to_address: Some("0xto".to_string()),
                value: "100".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 0,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0x".to_string(),
            },
            // Tx 1: block 100 (in range) ✓
            MempoolTransaction {
                hash: "0x02".to_string(),
                block_number: Some(100),
                timestamp_ms: 1001,
                from_address: "0xfrom".to_string(),
                to_address: None,
                value: "200".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 1,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0x".to_string(),
            },
            // Tx 2: block 102 (in range) ✓
            MempoolTransaction {
                hash: "0x03".to_string(),
                block_number: Some(102),
                timestamp_ms: 1002,
                from_address: "0xfrom".to_string(),
                to_address: Some("0xto".to_string()),
                value: "300".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 2,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0x".to_string(),
            },
            // Tx 3: NULL block_number (no block, excluded)
            MempoolTransaction {
                hash: "0x04".to_string(),
                block_number: None,
                timestamp_ms: 1003,
                from_address: "0xfrom".to_string(),
                to_address: Some("0xto".to_string()),
                value: "400".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 3,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0x".to_string(),
            },
            // Tx 4: block 103 (after range)
            MempoolTransaction {
                hash: "0x05".to_string(),
                block_number: Some(103),
                timestamp_ms: 1004,
                from_address: "0xfrom".to_string(),
                to_address: Some("0xto".to_string()),
                value: "500".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 4,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0x".to_string(),
            },
        ];

        let filtered = filter_by_block_range(txs, 100, 102);

        assert_eq!(filtered.len(), 2, "should keep 2 transactions in range");
        assert_eq!(filtered[0].hash, "0x02", "first kept tx has block 100");
        assert_eq!(filtered[1].hash, "0x03", "second kept tx has block 102");
    }
}
