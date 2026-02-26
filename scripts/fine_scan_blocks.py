#!/usr/bin/env python3
"""Fine-grained scan around candidate blocks with high cross-DEX spread."""

import os
import sys
import time
import json
import requests
from dotenv import load_dotenv

load_dotenv()
RPC_URL = os.environ.get("MEV_RPC_URL")

UNI_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_POOL = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"
RESERVES_SELECTOR = "0x0902f1ac"

def get_reserves(pool, block_hex):
    payload = {
        "jsonrpc": "2.0", "id": 1,
        "method": "eth_call",
        "params": [{"to": pool, "data": RESERVES_SELECTOR}, block_hex]
    }
    resp = requests.post(RPC_URL, json=payload, timeout=30)
    data = resp.json()
    result = data.get("result", "0x")
    raw = result[2:]
    if len(raw) < 128:
        return None, None
    return int(raw[0:64], 16), int(raw[64:128], 16)

def get_block_info(block_hex):
    payload = {
        "jsonrpc": "2.0", "id": 1,
        "method": "eth_getBlockByNumber",
        "params": [block_hex, True]
    }
    resp = requests.post(RPC_URL, json=payload, timeout=30)
    data = resp.json()
    return data.get("result", {})

def spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1):
    p_a = uni_r1 * sushi_r0
    p_b = sushi_r1 * uni_r0
    min_p = min(p_a, p_b)
    if min_p == 0:
        return 0.0
    return abs(p_a - p_b) * 10000 / min_p

# Fine-grained scan around blocks with known high spreads
candidates = [
    (16803100, 16803400),  # USDC depeg peak
    (15537300, 15537500),  # The Merge
]

high_blocks = []

for start, end in candidates:
    print(f"\n=== Scanning blocks {start}-{end} ===")
    for block in range(start, end):
        block_hex = hex(block)
        uni_r0, uni_r1 = get_reserves(UNI_POOL, block_hex)
        sushi_r0, sushi_r1 = get_reserves(SUSHI_POOL, block_hex)
        if uni_r0 is None or sushi_r0 is None:
            continue
        s = spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1)
        marker = " ***" if s >= 60 else ""
        print(f"  block={block} spread={s:.2f} bps{marker}")
        if s >= 60:
            high_blocks.append({
                "block": block,
                "spread_bps": round(s, 2),
                "uni_r0": uni_r0,
                "uni_r1": uni_r1,
                "sushi_r0": sushi_r0,
                "sushi_r1": sushi_r1,
            })
        time.sleep(0.12)

# For found blocks, get base fee and transaction details
print(f"\n\n=== HIGH SPREAD BLOCKS (>= 60 bps) ===")
for entry in high_blocks:
    block = entry["block"]
    block_hex = hex(block)
    info = get_block_info(block_hex)
    base_fee_hex = info.get("baseFeePerGas", "0x0")
    base_fee = int(base_fee_hex, 16)
    timestamp = int(info.get("timestamp", "0x0"), 16)
    tx_count = len(info.get("transactions", []))
    
    # Find first few txs (low tx_index)
    txs = info.get("transactions", [])
    early_txs = []
    for i, tx in enumerate(txs[:5]):
        early_txs.append({
            "tx_index": i,
            "tx_hash": tx.get("hash", ""),
            "from": tx.get("from", ""),
            "to": tx.get("to", ""),
        })
    
    entry["base_fee"] = base_fee
    entry["timestamp"] = timestamp
    entry["tx_count"] = tx_count
    entry["early_txs"] = early_txs
    
    print(f"\nblock={block} spread={entry['spread_bps']} bps base_fee={base_fee/1e9:.2f} Gwei ts={timestamp} txs={tx_count}")
    for tx in early_txs:
        print(f"  tx[{tx['tx_index']}] {tx['tx_hash'][:18]}... to={tx['to']}")
    time.sleep(0.5)

with open("/tmp/fine_scan_results.json", "w") as f:
    json.dump(high_blocks, f, indent=2, default=str)
print(f"\nSaved {len(high_blocks)} results to /tmp/fine_scan_results.json")
