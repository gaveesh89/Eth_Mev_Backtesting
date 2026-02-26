//! JSON-RPC client for Ethereum nodes (runs in the browser via `gloo-net`).

use gloo_net::http::Request;
use serde_json::{json, Value};

use crate::types::{RpcBlock, RpcReceipt};

/// Make a single JSON-RPC call and return the `result` field.
async fn rpc_call(url: &str, method: &str, params: &[Value]) -> Result<Value, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let resp = Request::post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .map_err(|e| format!("request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    if !resp.ok() {
        return Err(format!("HTTP {} from RPC", resp.status()));
    }

    let json: Value = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {e}"))?;

    if let Some(err) = json.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown RPC error");
        return Err(format!("RPC error: {msg}"));
    }

    Ok(json.get("result").cloned().unwrap_or(Value::Null))
}

/// Fetch a full block (with transaction objects) by number.
pub async fn fetch_block(url: &str, block_number: u64) -> Result<RpcBlock, String> {
    let hex = format!("0x{block_number:x}");
    let result = rpc_call(url, "eth_getBlockByNumber", &[json!(hex), json!(true)]).await?;

    if result.is_null() {
        return Err(format!("block {block_number} not found"));
    }

    serde_json::from_value(result).map_err(|e| format!("block deserialize: {e}"))
}

/// Fetch all transaction receipts for a given block.
///
/// Tries `eth_getBlockReceipts` first (supported by Alchemy, Erigon, etc.).
/// Falls back to individual `eth_getTransactionReceipt` calls on error.
pub async fn fetch_block_receipts(
    url: &str,
    block: &RpcBlock,
) -> Result<Vec<RpcReceipt>, String> {
    let block_hex = block
        .number
        .as_deref()
        .unwrap_or("0x0");

    // Try batch method first
    let batch_result = rpc_call(url, "eth_getBlockReceipts", &[json!(block_hex)]).await;

    match batch_result {
        Ok(val) if !val.is_null() => {
            let receipts: Vec<RpcReceipt> =
                serde_json::from_value(val).map_err(|e| format!("receipts deserialize: {e}"))?;
            Ok(receipts)
        }
        _ => {
            // Fallback: fetch each receipt individually
            let mut receipts = Vec::with_capacity(block.transactions.len());
            for tx in &block.transactions {
                if let Some(hash) = &tx.hash {
                    let result =
                        rpc_call(url, "eth_getTransactionReceipt", &[json!(hash)]).await?;
                    if !result.is_null() {
                        let r: RpcReceipt = serde_json::from_value(result)
                            .map_err(|e| format!("receipt deserialize: {e}"))?;
                        receipts.push(r);
                    }
                }
            }
            Ok(receipts)
        }
    }
}

/// Quick connectivity test — returns the latest block number.
pub async fn test_connection(url: &str) -> Result<u64, String> {
    let result = rpc_call(url, "eth_blockNumber", &[]).await?;
    let hex = result.as_str().unwrap_or("0x0");
    let s = hex.strip_prefix("0x").unwrap_or(hex);
    u64::from_str_radix(s, 16).map_err(|e| format!("parse block number: {e}"))
}
