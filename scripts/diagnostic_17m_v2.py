#!/usr/bin/env python3
"""Diagnostic v2 — focused investigation of scanner gaps.

Key questions:
1. Are there V2 Swap events on the scanned pools? (per-block to avoid Alchemy range limits)
2. Is the reserve decoding correct?
3. What are the actual spreads between Uni V2 and Sushi?
4. What does the V3 vs V2 landscape look like?
5. Is there actual MEV activity the scanner should detect?
"""
import json
import os
import sqlite3
import sys
import time

import requests

RPC = os.environ.get("MEV_RPC_URL")
if not RPC:
    print("ERROR: Set MEV_RPC_URL"); sys.exit(1)

DB_PATH = "data/mev.sqlite"

V2_SWAP = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
V2_SYNC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
V3_SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"

UNIV2_WETH_USDC = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_WETH_USDC = "0x397FF1542f962076d0BFE58eA045FfA2d347ACa0"
UNIV2_WETH_USDT = "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"
SUSHI_WETH_USDT = "0x06da0fd433C1A5d7a4faa01111c044910A184553"
UNIV2_WETH_DAI  = "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"
SUSHI_WETH_DAI  = "0xC3D03e4F041Fd4cD388c549Ee2A29a9E5075882f"

UNIV3_WETH_USDC_005 = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"

ALL_SCANNED_POOLS = [
    ("UniV2 WETH/USDC", UNIV2_WETH_USDC),
    ("Sushi WETH/USDC", SUSHI_WETH_USDC),
    ("UniV2 WETH/USDT", UNIV2_WETH_USDT),
    ("Sushi WETH/USDT", SUSHI_WETH_USDT),
    ("UniV2 WETH/DAI",  UNIV2_WETH_DAI),
    ("Sushi WETH/DAI",  SUSHI_WETH_DAI),
]

def rpc_call(method, params):
    r = requests.post(RPC, json={"jsonrpc":"2.0","id":1,"method":method,"params":params})
    resp = r.json()
    if "error" in resp:
        print(f"  RPC Error: {resp['error']}")
        return None
    return resp.get("result")

def hex_to_int(h):
    return int(h, 16) if h and h != "0x" else 0

def decode_v2_reserves(storage_hex):
    """Decode packed (reserve0, reserve1, blockTimestampLast) from V2 slot 8."""
    val = hex_to_int(storage_hex)
    mask112 = (1 << 112) - 1
    reserve0 = val & mask112                    # lowest 112 bits
    reserve1 = (val >> 112) & mask112           # next 112 bits
    ts_last  = (val >> 224) & 0xFFFFFFFF        # highest 32 bits
    return reserve0, reserve1, ts_last


# ===== DIAGNOSTIC 1: Per-block swap event counts =====
print("=" * 70)
print("DIAGNOSTIC 1: Per-block swap events on scanned pools")
print("=" * 70)

pool_addrs = [addr for _, addr in ALL_SCANNED_POOLS]
for bn in [17000000, 17000001, 17000005, 17000010, 17000015, 17000020]:
    logs = rpc_call("eth_getLogs", [{
        "fromBlock": hex(bn), "toBlock": hex(bn),
        "address": pool_addrs,
        "topics": [[V2_SWAP, V2_SYNC]]  # OR filter: Swap OR Sync
    }])
    swap_count = sum(1 for l in (logs or []) if l["topics"][0] == V2_SWAP)
    sync_count = sum(1 for l in (logs or []) if l["topics"][0] == V2_SYNC)
    
    # Also count all V2 swaps (any pool)
    all_logs = rpc_call("eth_getLogs", [{
        "fromBlock": hex(bn), "toBlock": hex(bn),
        "topics": [V2_SWAP]
    }])
    all_v2 = len(all_logs) if all_logs else 0
    
    # Also count V3 swaps
    v3_logs = rpc_call("eth_getLogs", [{
        "fromBlock": hex(bn), "toBlock": hex(bn),
        "topics": [V3_SWAP]
    }])
    all_v3 = len(v3_logs) if v3_logs else 0
    
    print(f"Block {bn}: Scanned-pool Swaps={swap_count} Syncs={sync_count} | All V2={all_v2} | All V3={all_v3}")
    time.sleep(0.5)


# ===== DIAGNOSTIC 2: Reserve state and spread calculation =====
print("\n" + "=" * 70)
print("DIAGNOSTIC 2: Pool reserves and cross-DEX spread at block 17000000")
print("=" * 70)

# Token ordering for known pools:
# UniV2 WETH/USDC (0xB4e1...): token0=USDC, token1=WETH
# Sushi WETH/USDC (0x397F...): token0=USDC, token1=WETH
# UniV2 WETH/USDT (0x0d4a...): token0=WETH, token1=USDT
# Sushi WETH/USDT (0x06da...): token0=WETH, token1=USDT
# UniV2 WETH/DAI (0xA478...):  token0=DAI, token1=WETH
# Sushi WETH/DAI (0xC3D0...):  token0=DAI, token1=WETH

POOL_CONFIG = {
    UNIV2_WETH_USDC.lower(): ("USDC", "WETH", 6, 18),  # token0=USDC, token1=WETH
    SUSHI_WETH_USDC.lower(): ("USDC", "WETH", 6, 18),
    UNIV2_WETH_USDT.lower(): ("WETH", "USDT", 18, 6),  # token0=WETH, token1=USDT
    SUSHI_WETH_USDT.lower(): ("WETH", "USDT", 18, 6),
    UNIV2_WETH_DAI.lower():  ("DAI", "WETH", 18, 18),   # token0=DAI, token1=WETH
    SUSHI_WETH_DAI.lower():  ("DAI", "WETH", 18, 18),
}

state_block = hex(16999999)  # block N-1 (pre-state)
prices = {}

for label, pool_addr in ALL_SCANNED_POOLS:
    raw = rpc_call("eth_getStorageAt", [pool_addr, "0x8", state_block])
    if not raw:
        print(f"{label}: failed to read")
        continue
    r0, r1, ts = decode_v2_reserves(raw)
    cfg = POOL_CONFIG[pool_addr.lower()]
    t0_name, t1_name, t0_dec, t1_dec = cfg
    
    t0_human = r0 / (10 ** t0_dec)
    t1_human = r1 / (10 ** t1_dec)
    
    # Calculate ETH price in USD
    if t0_name == "WETH":
        eth_amount = t0_human
        usd_amount = t1_human
    else:
        eth_amount = t1_human
        usd_amount = t0_human
    
    eth_price = usd_amount / eth_amount if eth_amount > 0 else 0
    
    print(f"\n{label} ({pool_addr[:10]}...):")
    print(f"  {t0_name}: {t0_human:,.2f} (raw: {r0})")
    print(f"  {t1_name}: {t1_human:,.6f} (raw: {r1})")
    print(f"  ETH price: ${eth_price:,.2f}")
    print(f"  TVL: ~${usd_amount * 2:,.2f}")
    print(f"  Last update timestamp: {ts}")
    prices[label] = eth_price

# Cross-DEX spreads
print("\n--- Cross-DEX Spreads ---")
pairs_to_check = [
    ("UniV2 WETH/USDC", "Sushi WETH/USDC"),
    ("UniV2 WETH/USDT", "Sushi WETH/USDT"),
    ("UniV2 WETH/DAI",  "Sushi WETH/DAI"),
]
for a_label, b_label in pairs_to_check:
    if a_label in prices and b_label in prices:
        pa, pb = prices[a_label], prices[b_label]
        if pa > 0 and pb > 0:
            spread_bps = abs(pa - pb) / min(pa, pb) * 10000
            print(f"  {a_label} vs {b_label}: {spread_bps:.2f} bps (${pa:.2f} vs ${pb:.2f})")
            if spread_bps > 60:
                print(f"    ✅ ABOVE 60 bps fee floor — should be detected!")
            elif spread_bps > 10:
                print(f"    ⚠️  Above 10 bps prefilter but below 60 bps fee floor")
            else:
                print(f"    ❌ Below 10 bps prefilter threshold")


# ===== DIAGNOSTIC 3: What V2 pools ARE active? =====
print("\n" + "=" * 70)
print("DIAGNOSTIC 3: Active V2 pools in block 17000000 (sample)")
print("=" * 70)

all_v2_swaps = rpc_call("eth_getLogs", [{
    "fromBlock": hex(17000000), "toBlock": hex(17000000),
    "topics": [V2_SWAP]
}])

if all_v2_swaps:
    from collections import Counter
    pool_counts = Counter(log["address"].lower() for log in all_v2_swaps)
    print(f"V2 Swap events in block 17000000: {len(all_v2_swaps)} across {len(pool_counts)} pools")
    for pool, cnt in pool_counts.most_common(10):
        is_scanned = "✅ SCANNED" if pool in [a.lower() for _, a in ALL_SCANNED_POOLS] else "❌ NOT SCANNED"
        print(f"  {pool}: {cnt} swaps — {is_scanned}")
else:
    print("No V2 swaps found!")


# ===== DIAGNOSTIC 4: V3 WETH/USDC price vs V2 =====
print("\n" + "=" * 70)
print("DIAGNOSTIC 4: V3 WETH/USDC pool state (for comparison)")
print("=" * 70)

# Read V3 slot0 for sqrtPriceX96
v3_slot0 = rpc_call("eth_getStorageAt", [UNIV3_WETH_USDC_005, "0x0", state_block])
if v3_slot0:
    val = hex_to_int(v3_slot0)
    sqrtPriceX96 = val & ((1 << 160) - 1)
    # V3 0.05% WETH/USDC: token0=USDC, token1=WETH
    # price = (sqrtPriceX96 / 2^96)^2 * 10^(dec0-dec1) = ... * 10^(6-18) = ... * 10^-12
    price_ratio = (sqrtPriceX96 / (2**96)) ** 2
    eth_price_v3 = 1 / (price_ratio * 1e-12) if price_ratio > 0 else 0
    print(f"V3 0.05% WETH/USDC ETH price: ${eth_price_v3:,.2f}")
    if "UniV2 WETH/USDC" in prices:
        v2_price = prices["UniV2 WETH/USDC"]
        v2_v3_spread = abs(v2_price - eth_price_v3) / min(v2_price, eth_price_v3) * 10000
        print(f"  V2-V3 spread: {v2_v3_spread:.2f} bps")
        print(f"  ⚠️  This V2↔V3 spread is INVISIBLE to the scanner (V3 not supported)")
else:
    print("Failed to read V3 slot0")


# ===== DIAGNOSTIC 5: Detailed MEV activity in block 17000000 =====
print("\n" + "=" * 70)
print("DIAGNOSTIC 5: MEV bot activity in block 17000000")
print("=" * 70)

block_data = rpc_call("eth_getBlockByNumber", [hex(17000000), True])
if block_data:
    txs = block_data.get("transactions", [])
    miner = block_data.get("miner", "").lower()
    
    # Get receipts for first few txs that interact with DEXes
    print(f"Total txs: {len(txs)}, Builder: {miner[:14]}...")
    
    # Check first 3 txs (often MEV in MEV-Boost blocks)
    for i, tx in enumerate(txs[:5]):
        tx_hash = tx["hash"]
        receipt = rpc_call("eth_getTransactionReceipt", [tx_hash])
        if not receipt:
            continue
        logs = receipt.get("logs", [])
        swap_logs = [l for l in logs if l["topics"] and l["topics"][0] in [V2_SWAP, V3_SWAP]]
        gas = hex_to_int(receipt.get("gasUsed", "0x0"))
        value_eth = hex_to_int(tx.get("value", "0x0")) / 1e18
        print(f"\n  TX #{i}: {tx_hash[:18]}...")
        print(f"    From: {tx['from'][:14]}... To: {(tx.get('to') or 'contract')[:14]}...")
        print(f"    Gas: {gas:,}, Value: {value_eth:.6f} ETH")
        print(f"    Swap events: {len(swap_logs)} ({'V2' if any(l['topics'][0]==V2_SWAP for l in swap_logs) else ''} {'V3' if any(l['topics'][0]==V3_SWAP for l in swap_logs) else ''})")
        if swap_logs:
            for sl in swap_logs[:3]:
                pool = sl["address"]
                is_scanned = pool.lower() in [a.lower() for _, a in ALL_SCANNED_POOLS]
                print(f"    → Pool {pool[:14]}... {'✅ scanned' if is_scanned else '❌ not scanned'}")
        time.sleep(0.3)

# ===== DIAGNOSTIC 6: CEX-DEX price comparison detail =====
print("\n" + "=" * 70)
print("DIAGNOSTIC 6: CEX vs DEX price at block 17000000")
print("=" * 70)

conn = sqlite3.connect(DB_PATH)
cur = conn.cursor()
cur.execute("SELECT close_micro FROM cex_prices WHERE pair='ETHUSDC' AND timestamp_s=1680911891")
row = cur.fetchone()
if row:
    cex_price = row[0] / 1e6
    v2_price = prices.get("UniV2 WETH/USDC", 0)
    sushi_price = prices.get("Sushi WETH/USDC", 0)
    
    print(f"CEX (Binance): ${cex_price:.2f}")
    print(f"UniV2 WETH/USDC: ${v2_price:.2f}")
    print(f"Sushi WETH/USDC: ${sushi_price:.2f}")
    
    if v2_price > 0:
        cex_v2_spread = abs(cex_price - v2_price) / min(cex_price, v2_price) * 10000
        print(f"CEX-V2 spread: {cex_v2_spread:.2f} bps")
        if cex_v2_spread > 30:
            print(f"  ⚠️  ABOVE 30 bps — CEX-DEX scanner should detect this!")
    if sushi_price > 0:
        cex_sushi_spread = abs(cex_price - sushi_price) / min(cex_price, sushi_price) * 10000
        print(f"CEX-Sushi spread: {cex_sushi_spread:.2f} bps")
conn.close()


print("\n" + "=" * 70)
print("FINAL SUMMARY")
print("=" * 70)
print("""
Key findings:
1. Pool activity: Are the 6 scanned V2 pools active at block 17M?
2. Reserve state: Are reserves non-trivial (>$1M TVL)?
3. Cross-DEX spread: Does it exceed the 60 bps fee floor?
4. V3 coverage gap: Are most swaps happening on V3 (not scanned)?
5. CEX-DEX alignment: Does CEX price differ enough from DEX?
6. Known MEV: Is the scanner missing active MEV txs?
""")
