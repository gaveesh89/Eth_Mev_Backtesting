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

#[cfg(test)]
mod tests {
    use super::*;

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
}
