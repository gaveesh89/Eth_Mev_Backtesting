#!/usr/bin/env python3
"""Scan depeg blocks 16817000-16817099 for Uni V2 vs Sushi V2 spreads >= 60 bps."""

import json
import os
import sys
import time
import requests
from dotenv import load_dotenv

load_dotenv()
RPC_URL = os.environ.get("MEV_RPC_URL")
if not RPC_URL:
    print("MEV_RPC_URL not set", file=sys.stderr)
    sys.exit(1)

UNI_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_POOL = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"

# getReserves() selector: 0x0902f1ac
RESERVES_SELECTOR = "0x0902f1ac"

def get_reserves(pool, block_hex):
    """Fetch reserves via eth_call at a specific block."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [
            {"to": pool, "data": RESERVES_SELECTOR},
            block_hex,
        ]
    }
    resp = requests.post(RPC_URL, json=payload, timeout=30)
    data = resp.json()
    if "error" in data and data["error"]:
        raise Exception(f"RPC error: {data['error']}")
    result = data["result"]
    raw = result[2:]  # strip 0x
    r0 = int(raw[0:64], 16)
    r1 = int(raw[64:128], 16)
    return r0, r1

def get_base_fee(block_hex):
    """Fetch baseFeePerGas for a block."""
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": [block_hex, False]
    }
    resp = requests.post(RPC_URL, json=payload, timeout=30)
    data = resp.json()
    result = data.get("result", {})
    bf_hex = result.get("baseFeePerGas", "0x0")
    return int(bf_hex, 16)

def compute_spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1):
    """Compute spread in basis points.
    
    For WETH/USDC pool:
    - token0 = USDC (lower address)
    - token1 = WETH (higher address)
    - price = r0/r1 (USDC per WETH, but we need to account for decimals)
    Actually, for spread computation we just need relative price ratios.
    price_uni = uni_r0 / uni_r1 (USDC per WETH)
    price_sushi = sushi_r0 / sushi_r1 (USDC per WETH)
    """
    if uni_r1 == 0 or sushi_r1 == 0:
        return 0.0
    # Using cross multiplication to avoid float precision issues:
    # spread_bps = |price_uni - price_sushi| / avg(price_uni, price_sushi) * 10000
    # = |uni_r0*sushi_r1 - sushi_r0*uni_r1| / ((uni_r0*sushi_r1 + sushi_r0*uni_r1)/2) * 10000
    cross1 = uni_r0 * sushi_r1
    cross2 = sushi_r0 * uni_r1
    if cross1 + cross2 == 0:
        return 0.0
    spread = abs(cross1 - cross2) * 20000 / (cross1 + cross2)
    return spread

print(f"Scanning blocks 16817000-16817099 for WETH/USDC spread...")
print(f"{'block':>10} {'uni_r0':>15} {'uni_r1':>15} {'sushi_r0':>15} {'sushi_r1':>15} {'spread_bps':>12} {'base_fee_gwei':>14}")
print("-" * 110)

high_spread_blocks = []

for block in range(16_817_000, 16_817_100):
    block_hex = hex(block)
    try:
        uni_r0, uni_r1 = get_reserves(UNI_POOL, block_hex)
        sushi_r0, sushi_r1 = get_reserves(SUSHI_POOL, block_hex)
        base_fee = get_base_fee(block_hex)
        spread = compute_spread_bps(uni_r0, uni_r1, sushi_r0, sushi_r1)
        
        marker = " ***" if spread >= 60 else ""
        print(f"{block:>10} {uni_r0:>15} {uni_r1:>15} {sushi_r0:>15} {sushi_r1:>15} {spread:>12.2f} {base_fee/1e9:>14.2f}{marker}")
        
        if spread >= 60:
            high_spread_blocks.append({
                "block": block,
                "spread_bps": round(spread, 2),
                "base_fee": base_fee,
                "uni_r0": uni_r0,
                "uni_r1": uni_r1,
                "sushi_r0": sushi_r0,
                "sushi_r1": sushi_r1,
            })
        
        # Rate limit
        time.sleep(0.15)
    except Exception as e:
        print(f"{block:>10} ERROR: {e}", file=sys.stderr)
        time.sleep(1)

print(f"\n=== RESULTS ===")
print(f"Blocks with >= 60 bps spread: {len(high_spread_blocks)}")
for entry in high_spread_blocks:
    print(f"  block={entry['block']} spread={entry['spread_bps']:.2f} bps base_fee={entry['base_fee']/1e9:.2f} Gwei")

if high_spread_blocks:
    with open("/tmp/high_spread_blocks.json", "w") as f:
        json.dump(high_spread_blocks, f, indent=2)
    print(f"\nSaved to /tmp/high_spread_blocks.json")
