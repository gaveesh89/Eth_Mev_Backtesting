//! # Binance Kline Fetcher
//!
//! Fetches historical kline (candlestick) data from the Binance public REST API
//! and converts prices to micro-USD integers for storage in the `cex_prices` table.
//!
//! Uses the public market-data endpoint (`data-api.binance.vision`) which requires
//! **no API key or signature**. Only 1-second klines are used for MEV backtesting
//! since CEX-DEX arbitrage detection needs sub-block price resolution.
//!
//! ## Rate Limits
//!
//! Each `/api/v3/klines` request costs weight 2. Binance allows 6000 weight per
//! minute on the public endpoint. With 1000 klines per request, fetching one full
//! day (~86 400 seconds) takes ~87 requests = 174 weight, well within limits.

use eyre::{eyre, Context, Result};
use tracing::{debug, info, warn};

/// Binance public market-data base URL (no API key required).
const BINANCE_BASE_URL: &str = "https://data-api.binance.vision";

/// Maximum klines returned per request by Binance.
const MAX_KLINES_PER_REQUEST: u64 = 1000;

/// A single kline row ready for database insertion.
///
/// Fields: `(pair, timestamp_s, open_micro, close_micro, high_micro, low_micro)`.
#[derive(Debug, Clone)]
pub struct CexKline {
    /// Trading pair name, e.g. `"ETHUSDC"`.
    pub pair: String,
    /// Kline open time in Unix seconds.
    pub timestamp_s: u64,
    /// Open price in micro-USD (price × 10⁶).
    pub open_micro: i64,
    /// Close price in micro-USD.
    pub close_micro: i64,
    /// High price in micro-USD.
    pub high_micro: i64,
    /// Low price in micro-USD.
    pub low_micro: i64,
}

/// Fetches Binance kline data for a time window and returns parsed rows.
///
/// Automatically paginates through the Binance API in chunks of 1000 klines.
/// Uses the `data-api.binance.vision` endpoint which requires no authentication.
///
/// # Arguments
///
/// * `symbol` — Binance symbol, e.g. `"ETHUSDC"`.
/// * `interval` — Kline interval, e.g. `"1s"`, `"1m"`.
/// * `start_time_s` — Start of the window in Unix seconds (inclusive).
/// * `end_time_s` — End of the window in Unix seconds (inclusive).
///
/// # Errors
///
/// Returns error if the HTTP request fails or the response is malformed.
#[tracing::instrument(skip_all, fields(symbol, interval, start_s = start_time_s, end_s = end_time_s))]
pub async fn fetch_binance_klines(
    symbol: &str,
    interval: &str,
    start_time_s: u64,
    end_time_s: u64,
) -> Result<Vec<CexKline>> {
    if end_time_s < start_time_s {
        return Err(eyre!(
            "end_time_s ({end_time_s}) must be >= start_time_s ({start_time_s})"
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .wrap_err("failed to build HTTP client")?;

    let pair = symbol.to_uppercase();
    let mut all_klines: Vec<CexKline> = Vec::new();
    let mut cursor_ms = start_time_s * 1000;
    let end_ms = end_time_s * 1000;
    let mut request_count = 0u32;

    loop {
        if cursor_ms > end_ms {
            break;
        }

        let url = format!(
            "{}/api/v3/klines?symbol={}&interval={}&startTime={}&endTime={}&limit={}",
            BINANCE_BASE_URL, pair, interval, cursor_ms, end_ms, MAX_KLINES_PER_REQUEST
        );

        debug!(
            request = request_count + 1,
            cursor_ms, "fetching klines batch"
        );

        let response = client
            .get(&url)
            .send()
            .await
            .wrap_err("Binance klines HTTP request failed")?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::IM_A_TEAPOT
        {
            // 429 or 418 — rate limited or IP banned
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            warn!(
                status = status.as_u16(),
                retry_after_s = retry_after,
                "Binance rate limit hit, waiting"
            );
            tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
            continue;
        }

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(eyre!(
                "Binance API returned HTTP {}: {}",
                status.as_u16(),
                body
            ));
        }

        let body = response
            .text()
            .await
            .wrap_err("failed to read Binance response body")?;

        let rows: Vec<Vec<serde_json::Value>> =
            serde_json::from_str(&body).wrap_err("failed to parse Binance klines JSON")?;

        if rows.is_empty() {
            debug!("no more klines returned, pagination complete");
            break;
        }

        let batch_len = rows.len();

        for row in &rows {
            if let Some(kline) = parse_kline_row(row, &pair) {
                all_klines.push(kline);
            }
        }

        // Advance cursor past the last kline's open time
        if let Some(last) = rows.last() {
            if let Some(open_time) = last.first().and_then(|v| v.as_u64()) {
                cursor_ms = open_time + 1; // +1ms to avoid re-fetching the last row
            } else {
                break;
            }
        }

        request_count += 1;

        // If we got fewer than the max, we've reached the end
        if (batch_len as u64) < MAX_KLINES_PER_REQUEST {
            break;
        }
    }

    info!(
        klines = all_klines.len(),
        requests = request_count,
        pair = pair.as_str(),
        "Binance kline fetch complete"
    );

    Ok(all_klines)
}

/// Parses a single Binance kline JSON array into a [`CexKline`].
///
/// Binance kline format:
/// ```text
/// [
///   open_time_ms,      // 0
///   "open_price",      // 1
///   "high_price",      // 2
///   "low_price",       // 3
///   "close_price",     // 4
///   "volume",          // 5
///   close_time_ms,     // 6
///   "quote_volume",    // 7
///   num_trades,        // 8
///   "taker_buy_base",  // 9
///   "taker_buy_quote", // 10
///   "ignore"           // 11
/// ]
/// ```
fn parse_kline_row(row: &[serde_json::Value], pair: &str) -> Option<CexKline> {
    if row.len() < 5 {
        return None;
    }

    let open_time_ms = row[0].as_u64()?;
    let open: f64 = row[1].as_str()?.parse().ok()?;
    let high: f64 = row[2].as_str()?.parse().ok()?;
    let low: f64 = row[3].as_str()?.parse().ok()?;
    let close: f64 = row[4].as_str()?.parse().ok()?;

    Some(CexKline {
        pair: pair.to_string(),
        timestamp_s: open_time_ms / 1000,
        open_micro: (open * 1_000_000.0) as i64,
        close_micro: (close * 1_000_000.0) as i64,
        high_micro: (high * 1_000_000.0) as i64,
        low_micro: (low * 1_000_000.0) as i64,
    })
}

/// Converts a vector of [`CexKline`] into the tuple format expected by
/// [`Store::insert_cex_prices`].
///
/// Returns `(pair, timestamp_s, open_micro, close_micro, high_micro, low_micro)` tuples.
pub fn klines_to_insert_tuples(klines: &[CexKline]) -> Vec<(&str, u64, i64, i64, i64, i64)> {
    klines
        .iter()
        .map(|k| {
            (
                k.pair.as_str(),
                k.timestamp_s,
                k.open_micro,
                k.close_micro,
                k.high_micro,
                k.low_micro,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kline_row_valid() {
        let row: Vec<serde_json::Value> = vec![
            serde_json::Value::Number(serde_json::Number::from(1678320000000u64)),
            serde_json::Value::String("1612.45".into()),
            serde_json::Value::String("1615.30".into()),
            serde_json::Value::String("1610.20".into()),
            serde_json::Value::String("1613.80".into()),
            serde_json::Value::String("1000.0".into()),
        ];

        let kline = parse_kline_row(&row, "ETHUSDC").expect("should parse");
        assert_eq!(kline.pair, "ETHUSDC");
        assert_eq!(kline.timestamp_s, 1678320000);
        assert_eq!(kline.open_micro, 1_612_450_000);
        assert_eq!(kline.close_micro, 1_613_800_000);
        assert_eq!(kline.high_micro, 1_615_300_000);
        assert_eq!(kline.low_micro, 1_610_200_000);
    }

    #[test]
    fn parse_kline_row_short_array() {
        let row: Vec<serde_json::Value> = vec![
            serde_json::Value::Number(serde_json::Number::from(1678320000000u64)),
            serde_json::Value::String("1612.45".into()),
        ];
        assert!(parse_kline_row(&row, "ETHUSDC").is_none());
    }

    #[test]
    fn parse_kline_row_bad_price() {
        let row: Vec<serde_json::Value> = vec![
            serde_json::Value::Number(serde_json::Number::from(1678320000000u64)),
            serde_json::Value::String("not-a-number".into()),
            serde_json::Value::String("1615.30".into()),
            serde_json::Value::String("1610.20".into()),
            serde_json::Value::String("1613.80".into()),
        ];
        assert!(parse_kline_row(&row, "ETHUSDC").is_none());
    }

    #[test]
    fn klines_to_tuples_roundtrip() {
        let klines = vec![CexKline {
            pair: "ETHUSDC".to_string(),
            timestamp_s: 1678320000,
            open_micro: 1_612_450_000,
            close_micro: 1_613_800_000,
            high_micro: 1_615_300_000,
            low_micro: 1_610_200_000,
        }];

        let tuples = klines_to_insert_tuples(&klines);
        assert_eq!(tuples.len(), 1);
        assert_eq!(tuples[0].0, "ETHUSDC");
        assert_eq!(tuples[0].1, 1678320000);
        assert_eq!(tuples[0].2, 1_612_450_000);
    }

    #[test]
    fn micro_usd_precision() {
        // Verify that typical ETH prices don't lose precision in micro-USD
        let price = 3587.42f64;
        let micro = (price * 1_000_000.0) as i64;
        assert_eq!(micro, 3_587_420_000);

        // Verify roundtrip
        let back = micro as f64 / 1_000_000.0;
        assert!((back - price).abs() < 0.01);
    }
}
