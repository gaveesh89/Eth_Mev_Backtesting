#!/usr/bin/env python3
"""Check Uni V2 and Sushi WETH/USDC reserves at several blocks to find spread."""

import urllib.request
import json
import os
import sys
import time

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

# getReserves() function selector
GET_RESERVES = "0x0902f1ac"

UNI_V2_WETH_USDC = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_WETH_USDC  = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"

def rpc(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(rpc_url, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read()).get("result")

def get_reserves(pool, block_hex):
    result = rpc("eth_call", [{"to": pool, "data": GET_RESERVES}, block_hex])
    if not result or result == "0x":
        return 0, 0
    d = result[2:]
    r0 = int(d[0:64], 16)
    r1 = int(d[64:128], 16)
    return r0, r1

# USDC < WETH by address (token0=USDC, token1=WETH)
# Actually check: USDC = 0xa0b8... WETH = 0xc02a...
# 0xa0... < 0xc0... so token0=USDC, token1=WETH

print("block | Uni_USDC | Uni_WETH | Sushi_USDC | Sushi_WETH | Uni_price | Sushi_price | spread_bps")
print("-"*120)

for block in range(16817000, 16817100, 5):
    block_hex = hex(block - 1)  # state BEFORE the block
    
    ur0, ur1 = get_reserves(UNI_V2_WETH_USDC, block_hex)
    sr0, sr1 = get_reserves(SUSHI_WETH_USDC, block_hex)
    
    if ur1 == 0 or sr1 == 0:
        print(f"{block} | SKIP (zero reserves)")
        continue
    
    # Price = USDC_reserve / WETH_reserve (adjust for decimals: USDC has 6, WETH has 18)
    uni_price = (ur0 * 1e12) / ur1  # USDC/WETH in same units
    sushi_price = (sr0 * 1e12) / sr1
    
    spread_bps = abs(uni_price - sushi_price) / min(uni_price, sushi_price) * 10000
    
    print(f"{block} | {ur0:>15,} | {ur1:>22,} | {sr0:>15,} | {sr1:>22,} | ${uni_price:.2f} | ${sushi_price:.2f} | {spread_bps:.1f}")
    
    time.sleep(0.3)
