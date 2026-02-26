#!/usr/bin/env python3
"""Quick scan of candidate block ranges for Uni V2 vs Sushi V2 WETH/USDC spread > 60 bps."""

import os
import sys
import time
import json
import requests
from dotenv import load_dotenv

load_dotenv()
RPC_URL = os.environ.get("MEV_RPC_URL")
if not RPC_URL:
    print("MEV_RPC_URL not set", file=sys.stderr)
    sys.exit(1)

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
    if "error" in data and data["error"]:
        return None, None
    result = data.get("result", "0x")
    raw = result[2:]
    if len(raw) < 128:
        return None, None
    r0 = int(raw[0:64], 16)
    r1 = int(raw[64:128], 16)
    return r0, r1

def spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1):
    if uni_r1 == 0 or sushi_r1 == 0 or uni_r0 == 0 or sushi_r0 == 0:
        return 0.0
    # p_a_num = r1a * r0b,  p_b_num = r1b * r0a  (matching Rust formula)
    p_a = uni_r1 * sushi_r0
    p_b = sushi_r1 * uni_r0
    min_p = min(p_a, p_b)
    if min_p == 0:
        return 0.0
    disc = abs(p_a - p_b)
    return (disc * 10000) / min_p  # Uses min(p) like Rust code

# Candidate ranges to check (sampling every 10 blocks for speed)
ranges = [
    # USDC depeg peak: March 11, 2023 evening
    ("USDC depeg peak (Mar 11)", 16_800_000, 16_805_000, 100),
    # Slightly before our window
    ("Pre-depeg window (Mar 12-13)", 16_810_000, 16_817_000, 100),
    # FTX crash: Nov 8, 2022
    ("FTX crash (Nov 8 2022)", 15_920_000, 15_925_000, 100),
    # Merge: Sep 15, 2022
    ("The Merge (Sep 15 2022)", 15_537_000, 15_538_000, 50),
    # Recent high-vol: Jan 2024
    ("High-vol Jan 2024", 18_900_000, 18_905_000, 100),
    # Terra/LUNA crash: May 9, 2022
    ("Terra crash (May 2022)", 14_740_000, 14_745_000, 100),
]

results = []

for name, start, end, step in ranges:
    print(f"\n=== {name}: blocks {start}-{end} (step={step}) ===")
    max_spread = 0.0
    max_block = start
    for block in range(start, end, step):
        block_hex = hex(block)
        uni_r0, uni_r1 = get_reserves(UNI_POOL, block_hex)
        sushi_r0, sushi_r1 = get_reserves(SUSHI_POOL, block_hex)
        if uni_r0 is None or sushi_r0 is None:
            print(f"  block={block} ERROR fetching reserves")
            time.sleep(0.5)
            continue
        s = spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1)
        if s > max_spread:
            max_spread = s
            max_block = block
        marker = " ***" if s >= 60 else ""
        if s >= 30 or block % 500 == 0:  # Only print notable ones
            print(f"  block={block} spread={s:.2f} bps{marker}")
        if s >= 60:
            results.append({"range": name, "block": block, "spread_bps": round(s, 2)})
        time.sleep(0.12)
    print(f"  MAX spread in range: {max_spread:.2f} bps at block {max_block}")

print(f"\n{'='*60}")
print(f"Blocks with >= 60 bps spread: {len(results)}")
for r in results:
    print(f"  {r['range']}: block={r['block']} spread={r['spread_bps']} bps")

if results:
    with open("/tmp/high_spread_candidates.json", "w") as f:
        json.dump(results, f, indent=2)
