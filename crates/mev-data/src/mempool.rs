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

    // Build candidate URLs (current + legacy layout):
    // - https://mempool-dumpster.flashbots.net/ethereum/mainnet/{YYYY-MM}/{YYYY-MM-DD}.parquet
    // - https://mempool-dumpster.flashbots.net/ethereum/mainnet/{YYYY-MM}/transactions/{YYYY-MM-DD}.parquet
    let year_month = date.format("%Y-%m").to_string();
    let date_str = date.format("%Y-%m-%d").to_string();
    let candidate_urls = [
        format!(
            "https://mempool-dumpster.flashbots.net/ethereum/mainnet/{}/{}.parquet",
            year_month, date_str
        ),
        format!(
            "https://mempool-dumpster.flashbots.net/ethereum/mainnet/{}/transactions/{}.parquet",
            year_month, date_str
        ),
    ];

    // Create async HTTP client
    let client = reqwest::Client::new();

    // Try candidate URLs until one succeeds
    let mut selected_url: Option<String> = None;
    let mut response_opt = None;
    let mut last_error: Option<eyre::Report> = None;

    for url in &candidate_urls {
        tracing::info!(url = %url, "attempting mempool-dumpster parquet download");

        let response = match client
            .get(url)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(error) => {
                last_error =
                    Some(eyre::eyre!(error).wrap_err(format!("failed to download from {}", url)));
                continue;
            }
        };

        if response.status().is_success() {
            selected_url = Some(url.clone());
            response_opt = Some(response);
            break;
        }

        last_error = Some(
            eyre::eyre!("HTTP status {}", response.status()).wrap_err(format!(
                "mempool-dumpster returned error status for {}",
                url
            )),
        );
    }

    let response = match response_opt {
        Some(resp) => resp,
        None => {
            return Err(last_error
                .unwrap_or_else(|| eyre::eyre!("no mempool-dumpster URL candidates succeeded")));
        }
    };

    let selected_url = selected_url.unwrap_or_else(|| "<unknown>".to_string());

    // content-length can be absent on some CDN responses; fall back to spinner.
    let total_size = response.content_length();

    let progress_bar = match total_size {
        Some(size) => {
            let pb = ProgressBar::new(size);
            pb.set_style(
                indicatif::ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .wrap_err("invalid progress bar template")?,
            );
            pb
        }
        None => {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .template("{spinner:.green} downloading... {bytes}")
                    .wrap_err("invalid spinner template")?,
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            pb
        }
    };

    // Stream response body and write to file
    let mut file = tokio::fs::File::create(&local_path)
        .await
        .wrap_err_with(|| format!("failed to create file: {}", local_path.display()))?;

    let mut stream = response.bytes_stream();
    let mut downloaded_bytes: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("failed to read response chunk")?;
        downloaded_bytes = downloaded_bytes.saturating_add(chunk.len() as u64);
        progress_bar.inc(chunk.len() as u64);

        file.write_all(&chunk)
            .await
            .wrap_err("failed to write to parquet file")?;
    }

    progress_bar.finish_with_message("download complete");

    tracing::info!(
        path = %local_path.display(),
        url = %selected_url,
        bytes = downloaded_bytes,
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
    use arrow::array::{
        Array, BinaryArray, FixedSizeBinaryArray, Int32Array, Int64Array, LargeBinaryArray,
        LargeStringArray, StringArray, TimestampMicrosecondArray, TimestampMillisecondArray,
        TimestampNanosecondArray, TimestampSecondArray, UInt32Array, UInt64Array,
    };
    use arrow::datatypes::DataType;
    use arrow::record_batch::RecordBatchReader;
    use eyre::ContextCompat;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    fn col_idx(schema: &arrow::datatypes::Schema, names: &[&str]) -> Result<usize> {
        for name in names {
            if let Ok(idx) = schema.index_of(name) {
                return Ok(idx);
            }
        }
        Err(eyre::eyre!(
            "none of columns {:?} found in parquet schema",
            names
        ))
    }

    fn array_string(array: &dyn Array, row_idx: usize, label: &str) -> Result<String> {
        fn to_0x_hex(bytes: &[u8]) -> String {
            let mut out = String::with_capacity(2 + bytes.len() * 2);
            out.push_str("0x");
            for b in bytes {
                out.push_str(&format!("{b:02x}"));
            }
            out
        }

        if array.is_null(row_idx) {
            return Ok(String::new());
        }

        match array.data_type() {
            DataType::Utf8 => Ok(array
                .as_any()
                .downcast_ref::<StringArray>()
                .context(format!("{label} column not Utf8"))?
                .value(row_idx)
                .to_string()),
            DataType::LargeUtf8 => Ok(array
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .context(format!("{label} column not LargeUtf8"))?
                .value(row_idx)
                .to_string()),
            DataType::Binary => Ok(to_0x_hex(
                array
                    .as_any()
                    .downcast_ref::<BinaryArray>()
                    .context(format!("{label} column not Binary"))?
                    .value(row_idx),
            )),
            DataType::LargeBinary => Ok(to_0x_hex(
                array
                    .as_any()
                    .downcast_ref::<LargeBinaryArray>()
                    .context(format!("{label} column not LargeBinary"))?
                    .value(row_idx),
            )),
            DataType::FixedSizeBinary(_) => Ok(to_0x_hex(
                array
                    .as_any()
                    .downcast_ref::<FixedSizeBinaryArray>()
                    .context(format!("{label} column not FixedSizeBinary"))?
                    .value(row_idx),
            )),
            DataType::UInt64 => Ok(array
                .as_any()
                .downcast_ref::<UInt64Array>()
                .context(format!("{label} column not UInt64"))?
                .value(row_idx)
                .to_string()),
            DataType::UInt32 => Ok(array
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context(format!("{label} column not UInt32"))?
                .value(row_idx)
                .to_string()),
            DataType::Int64 => Ok(array
                .as_any()
                .downcast_ref::<Int64Array>()
                .context(format!("{label} column not Int64"))?
                .value(row_idx)
                .to_string()),
            DataType::Int32 => Ok(array
                .as_any()
                .downcast_ref::<Int32Array>()
                .context(format!("{label} column not Int32"))?
                .value(row_idx)
                .to_string()),
            DataType::Timestamp(_, _) => {
                if let Some(col) = array.as_any().downcast_ref::<TimestampMillisecondArray>() {
                    return Ok(col.value(row_idx).to_string());
                }
                if let Some(col) = array.as_any().downcast_ref::<TimestampMicrosecondArray>() {
                    return Ok(col.value(row_idx).to_string());
                }
                if let Some(col) = array.as_any().downcast_ref::<TimestampNanosecondArray>() {
                    return Ok(col.value(row_idx).to_string());
                }
                if let Some(col) = array.as_any().downcast_ref::<TimestampSecondArray>() {
                    return Ok(col.value(row_idx).to_string());
                }
                Err(eyre::eyre!("unsupported timestamp storage for {label}"))
            }
            other => Err(eyre::eyre!("unsupported datatype for {label}: {:?}", other)),
        }
    }

    fn array_u64_opt(array: &dyn Array, row_idx: usize, label: &str) -> Result<Option<u64>> {
        if array.is_null(row_idx) {
            return Ok(None);
        }

        match array.data_type() {
            DataType::UInt64 => Ok(Some(
                array
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .context(format!("{label} column not UInt64"))?
                    .value(row_idx),
            )),
            DataType::UInt32 => Ok(Some(
                array
                    .as_any()
                    .downcast_ref::<UInt32Array>()
                    .context(format!("{label} column not UInt32"))?
                    .value(row_idx) as u64,
            )),
            DataType::Int64 => Ok(Some(
                array
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .context(format!("{label} column not Int64"))?
                    .value(row_idx) as u64,
            )),
            DataType::Int32 => Ok(Some(
                array
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .context(format!("{label} column not Int32"))?
                    .value(row_idx) as u64,
            )),
            DataType::Utf8 | DataType::LargeUtf8 => {
                let s = array_string(array, row_idx, label)?;
                let parsed = s.parse::<u64>().wrap_err_with(|| {
                    format!("failed to parse {label} as u64 from string value: {s}")
                })?;
                Ok(Some(parsed))
            }
            DataType::Timestamp(_, _) => {
                let ts = array_string(array, row_idx, label)?;
                let parsed = ts
                    .parse::<u64>()
                    .wrap_err_with(|| format!("failed to parse {label} timestamp as u64: {ts}"))?;
                Ok(Some(parsed))
            }
            other => Err(eyre::eyre!("unsupported datatype for {label}: {:?}", other)),
        }
    }

    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open parquet file: {}", path.display()))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .wrap_err("failed to parse parquet metadata")?;

    let reader = builder
        .build()
        .wrap_err("failed to build parquet record batch reader")?;

    let schema = reader.schema();

    // Legacy schema aliases + current mempool-dumpster schema aliases.
    let hash_idx = col_idx(&schema, &["hash"])?;
    let block_number_idx = col_idx(&schema, &["block_number", "includedAtBlockHeight"])?;
    let timestamp_ms_idx = col_idx(
        &schema,
        &["timestamp_ms", "timestamp", "includedBlockTimestamp"],
    )?;
    let from_address_idx = col_idx(&schema, &["from_address", "from"])?;
    let to_address_idx = col_idx(&schema, &["to_address", "to"])?;
    let value_idx = col_idx(&schema, &["value"])?;
    let gas_limit_idx = col_idx(&schema, &["gas_limit", "gas"])?;
    let gas_price_idx = col_idx(&schema, &["gas_price", "gasPrice"])?;
    let max_fee_per_gas_idx = col_idx(&schema, &["max_fee_per_gas", "gasFeeCap"])?;
    let max_priority_fee_per_gas_idx =
        col_idx(&schema, &["max_priority_fee_per_gas", "gasTipCap"])?;
    let nonce_idx = col_idx(&schema, &["nonce"])?;
    let input_data_idx = col_idx(&schema, &["input_data", "data", "data4Bytes"])?;
    let tx_type_idx = col_idx(&schema, &["tx_type", "txType"])?;
    let raw_tx_idx = col_idx(&schema, &["raw_tx", "rawTx"])?;

    let mut txs = Vec::new();

    // Stream record batches
    for batch_result in reader {
        let batch = batch_result.wrap_err("failed to read record batch")?;

        let hash_col = batch.column(hash_idx).as_ref();
        let block_number_col = batch.column(block_number_idx).as_ref();
        let timestamp_ms_col = batch.column(timestamp_ms_idx).as_ref();
        let from_address_col = batch.column(from_address_idx).as_ref();
        let to_address_col = batch.column(to_address_idx).as_ref();
        let value_col = batch.column(value_idx).as_ref();
        let gas_limit_col = batch.column(gas_limit_idx).as_ref();
        let gas_price_col = batch.column(gas_price_idx).as_ref();
        let max_fee_per_gas_col = batch.column(max_fee_per_gas_idx).as_ref();
        let max_priority_fee_per_gas_col = batch.column(max_priority_fee_per_gas_idx).as_ref();
        let nonce_col = batch.column(nonce_idx).as_ref();
        let input_data_col = batch.column(input_data_idx).as_ref();
        let tx_type_col = batch.column(tx_type_idx).as_ref();
        let raw_tx_col = batch.column(raw_tx_idx).as_ref();

        // Map rows in batch to MempoolTransaction structs
        for row_idx in 0..batch.num_rows() {
            let tx_type_val = array_u64_opt(tx_type_col, row_idx, "tx_type")?.unwrap_or(0) as u32;
            let tx = MempoolTransaction {
                hash: array_string(hash_col, row_idx, "hash")?,
                block_number: array_u64_opt(block_number_col, row_idx, "block_number")?,
                timestamp_ms: array_u64_opt(timestamp_ms_col, row_idx, "timestamp")?
                    .unwrap_or_default(),
                from_address: array_string(from_address_col, row_idx, "from_address")?,
                to_address: if to_address_col.is_null(row_idx) {
                    None
                } else {
                    Some(array_string(to_address_col, row_idx, "to_address")?)
                },
                value: array_string(value_col, row_idx, "value")?,
                gas_limit: array_u64_opt(gas_limit_col, row_idx, "gas_limit")?.unwrap_or(0),
                gas_price: array_string(gas_price_col, row_idx, "gas_price")?,
                max_fee_per_gas: array_string(max_fee_per_gas_col, row_idx, "max_fee_per_gas")?,
                max_priority_fee_per_gas: array_string(
                    max_priority_fee_per_gas_col,
                    row_idx,
                    "max_priority_fee_per_gas",
                )?,
                nonce: array_u64_opt(nonce_col, row_idx, "nonce")?.unwrap_or(0),
                input_data: {
                    let data = array_string(input_data_col, row_idx, "input_data")?;
                    if data.is_empty() {
                        "0x".to_string()
                    } else {
                        data
                    }
                },
                tx_type: tx_type_val,
                raw_tx: array_string(raw_tx_col, row_idx, "raw_tx")?,
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
