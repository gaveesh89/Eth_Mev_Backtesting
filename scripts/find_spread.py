#!/usr/bin/env python3
"""Query reserves on Uni V2 and Sushi WETH/USDC for each block in the depeg window,
calculate spread, and identify blocks where spread > 10 bps (scanner threshold)."""

import urllib.request
import json
import os
import sys
import time

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

# Uni V2 WETH/USDC pair: 0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc
# Sushi WETH/USDC pair: 0x397FF1542f962076d0bFe58EA045FfA2d347aca0
UNI_PAIR = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_PAIR = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"

# getReserves() slot layout: slot 8 contains reserve0 (112 bit) + reserve1 (112 bit) + timestamp (32 bit)
# Alternatively use eth_call with getReserves()
GET_RESERVES_SIG = "0x0902f1ac"  # getReserves()

def rpc(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(rpc_url, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read()).get("result")

def get_reserves(pair_address, block_hex):
    """Call getReserves() on a UniV2 pair contract."""
    result = rpc("eth_call", [{"to": pair_address, "data": GET_RESERVES_SIG}, block_hex])
    if not result or len(result) < 194:
        return None, None
    # Return is (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
    d = result[2:]  # strip 0x
    reserve0 = int(d[0:64], 16)
    reserve1 = int(d[64:128], 16)
    return reserve0, reserve1

# USDC is token0 (lower address), WETH is token1 for UniV2 WETH/USDC
# Price = reserve0/reserve1 (USDC per WETH, accounting for decimals)

print("block | uni_price | sushi_price | spread_bps | profitable?")
print("-" * 70)

good_blocks = []

for block in range(16817000, 16817100, 5):  # Sample every 5th block
    block_hex = hex(block - 1)  # scanner uses prior block state
    
    ur0, ur1 = get_reserves(UNI_PAIR, block_hex)
    sr0, sr1 = get_reserves(SUSHI_PAIR, block_hex)
    
    if not ur0 or not sr0 or ur1 == 0 or sr1 == 0:
        print(f"{block} | ERROR reading reserves")
        continue
    
    # USDC/WETH price (USDC per WETH in 6-decimal terms)
    # token0 = USDC (6 dec), token1 = WETH (18 dec)
    # price = (reserve0 * 1e18) / (reserve1 * 1e6) = reserve0 * 1e12 / reserve1
    uni_price = ur0 * 1e12 / ur1
    sushi_price = sr0 * 1e12 / sr1
    
    spread = abs(uni_price - sushi_price)
    mid = (uni_price + sushi_price) / 2
    spread_bps = (spread / mid) * 10000
    
    profitable = "YES" if spread_bps > 10 else "no"
    print(f"{block} | {uni_price:.2f} | {sushi_price:.2f} | {spread_bps:.2f} | {profitable}")
    
    if spread_bps > 10:
        good_blocks.append((block, uni_price, sushi_price, spread_bps))
    
    time.sleep(0.15)

print(f"\n=== Blocks with spread > 10 bps: {len(good_blocks)} ===")
for b, up, sp, sbps in good_blocks:
    direction = "buy_uni_sell_sushi" if up < sp else "buy_sushi_sell_uni"
    print(f"  block={b} uni=${up:.2f} sushi=${sp:.2f} spread={sbps:.2f}bps direction={direction}")

# Also check the tx at index 0-3 in the strongest blocks (MEV typically early)
if good_blocks:
    print("\n=== Checking early txs in strongest blocks ===")
    top = sorted(good_blocks, key=lambda x: -x[3])[:5]
    for b, up, sp, sbps in top:
        block_data = rpc("eth_getBlockByNumber", [hex(b), True])
        if not block_data:
            continue
        txs = block_data.get("transactions", [])
        print(f"\nBlock {b} (spread={sbps:.2f}bps): {len(txs)} total txs")
        for i, tx in enumerate(txs[:5]):
            print(f"  tx_idx={i} from={tx['from'][:12]}... to={tx.get('to','(create)')[:12] if tx.get('to') else '(create)'}... gas={int(tx.get('gas','0x0'),16)} hash={tx['hash']}")
        time.sleep(0.15)
