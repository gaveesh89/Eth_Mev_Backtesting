#!/usr/bin/env python3
"""Diagnostic script to verify swap events and pool state at block 17M.

Checks:
1. V2 Swap events in blocks 17000000-17000010
2. V3 Swap events in the same blocks
3. UniV2 WETH/USDC pool reserve magnitudes
4. CEX timestamp alignment
5. Known MEV builder payments
"""
import json
import os
import sqlite3
import sys
import time

import requests

RPC = os.environ.get("MEV_RPC_URL")
if not RPC:
    print("ERROR: Set MEV_RPC_URL")
    sys.exit(1)

DB_PATH = "data/mev.sqlite"

# Event signatures
V2_SWAP_TOPIC = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"

# Known pool addresses
UNIV2_WETH_USDC = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_WETH_USDC = "0x397FF1542f962076d0BFE58eA045FfA2d347ACa0"
UNIV3_WETH_USDC_005 = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"  # 0.05% fee
UNIV3_WETH_USDC_030 = "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"  # 0.30% fee

def rpc_call(method, params):
    r = requests.post(RPC, json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    return r.json().get("result")

def get_logs(from_block, to_block, topics, addresses=None):
    params = {
        "fromBlock": hex(from_block),
        "toBlock": hex(to_block),
        "topics": topics,
    }
    if addresses:
        params["address"] = addresses
    return rpc_call("eth_getLogs", [params]) or []

def hex_to_int(h):
    return int(h, 16) if h else 0


print("=" * 70)
print("DIAGNOSTIC 1: V2 Swap events in blocks 17000000-17000010")
print("=" * 70)

v2_pools = [UNIV2_WETH_USDC, SUSHI_WETH_USDC]
v2_swap_logs = get_logs(17000000, 17000010, [V2_SWAP_TOPIC], v2_pools)
print(f"V2 Swap events (WETH/USDC on Uni+Sushi): {len(v2_swap_logs)}")
for log in v2_swap_logs[:5]:
    bn = hex_to_int(log["blockNumber"])
    tx = log["transactionHash"]
    addr = log["address"]
    print(f"  Block {bn} | Pool {addr[:10]}... | TX {tx[:18]}...")

# Also check ALL V2 Swap events (any pool)
all_v2_swaps = get_logs(17000000, 17000010, [V2_SWAP_TOPIC])
print(f"\nALL V2 Swap events (any pool): {len(all_v2_swaps)}")

# Count unique pools
unique_v2_pools = set(log["address"].lower() for log in all_v2_swaps)
print(f"Unique V2 pools with Swap events: {len(unique_v2_pools)}")
if UNIV2_WETH_USDC.lower() in unique_v2_pools:
    print(f"  ✅ UniV2 WETH/USDC pool is active")
else:
    print(f"  ❌ UniV2 WETH/USDC pool has ZERO swap events")

if SUSHI_WETH_USDC.lower() in unique_v2_pools:
    print(f"  ✅ SushiSwap WETH/USDC pool is active")
else:
    print(f"  ❌ SushiSwap WETH/USDC pool has ZERO swap events")

time.sleep(0.5)

print("\n" + "=" * 70)
print("DIAGNOSTIC 2: V3 Swap events in blocks 17000000-17000010")
print("=" * 70)

v3_swaps = get_logs(17000000, 17000010, [V3_SWAP_TOPIC])
print(f"ALL V3 Swap events: {len(v3_swaps)}")
unique_v3_pools = set(log["address"].lower() for log in v3_swaps)
print(f"Unique V3 pools with Swap events: {len(unique_v3_pools)}")

# Check specific V3 WETH/USDC pools
for label, addr in [("V3 0.05% WETH/USDC", UNIV3_WETH_USDC_005), ("V3 0.30% WETH/USDC", UNIV3_WETH_USDC_030)]:
    count = sum(1 for log in v3_swaps if log["address"].lower() == addr.lower())
    if count > 0:
        print(f"  ✅ {label}: {count} swaps")
    else:
        print(f"  ❌ {label}: 0 swaps")

time.sleep(0.5)

print("\n" + "=" * 70)
print("DIAGNOSTIC 3: V2 Sync events (used by intra-block scanner)")
print("=" * 70)

v2_sync_logs = get_logs(17000000, 17000010, [V2_SYNC_TOPIC], v2_pools)
print(f"V2 Sync events (WETH/USDC on Uni+Sushi): {len(v2_sync_logs)}")

time.sleep(0.5)

print("\n" + "=" * 70)
print("DIAGNOSTIC 4: UniV2 WETH/USDC pool reserves at block 17000000")
print("=" * 70)

# Read storage slot 8 (packed reserves)
for label, pool_addr in [("UniV2", UNIV2_WETH_USDC), ("SushiV2", SUSHI_WETH_USDC)]:
    raw = rpc_call("eth_getStorageAt", [pool_addr, "0x8", hex(16999999)])
    if raw:
        val = int(raw, 16)
        reserve1 = val & ((1 << 112) - 1)
        reserve0 = (val >> 112) & ((1 << 112) - 1)
        # UniV2 WETH/USDC: token0=USDC (6 dec), token1=WETH (18 dec)
        usdc_reserve = reserve0 / 1e6
        weth_reserve = reserve1 / 1e18
        implied_price = usdc_reserve / weth_reserve if weth_reserve > 0 else 0
        print(f"{label} WETH/USDC at block 16999999:")
        print(f"  reserve0 (USDC): {usdc_reserve:,.2f} USDC")
        print(f"  reserve1 (WETH): {weth_reserve:,.6f} WETH")
        print(f"  Implied price: ${implied_price:,.2f}/ETH")
        print(f"  TVL estimate: ${usdc_reserve * 2:,.2f}")
    else:
        print(f"{label}: failed to read reserves")

time.sleep(0.5)

print("\n" + "=" * 70)
print("DIAGNOSTIC 5: CEX timestamp alignment check")
print("=" * 70)

conn = sqlite3.connect(DB_PATH)
cur = conn.cursor()

# Get block timestamps for 17000000-17000010
cur.execute("SELECT block_number, timestamp FROM blocks WHERE block_number BETWEEN 17000000 AND 17000010 ORDER BY block_number")
blocks = cur.fetchall()

for bn, ts in blocks[:5]:
    # Find nearest CEX price
    cur.execute("""
        SELECT pair, timestamp_s, close_micro,
               ABS(timestamp_s - ?) AS delta_s
        FROM cex_prices
        WHERE pair = 'ETHUSDC'
        ORDER BY ABS(timestamp_s - ?) ASC
        LIMIT 1
    """, (ts, ts))
    row = cur.fetchone()
    if row:
        pair, cex_ts, close_micro, delta = row
        cex_price = close_micro / 1e6
        print(f"Block {bn}: ts={ts}, nearest CEX ts={cex_ts}, delta={delta}s, CEX price=${cex_price:.2f}")
        if delta > 3:
            print(f"  ⚠️  Delta > 3s — would be rejected as StaleCexData!")
    else:
        print(f"Block {bn}: ts={ts}, NO CEX DATA FOUND")

# Check overall CEX coverage
cur.execute("SELECT MIN(timestamp_s), MAX(timestamp_s), COUNT(*) FROM cex_prices WHERE pair='ETHUSDC'")
cex_min, cex_max, cex_count = cur.fetchone()
cur.execute("SELECT MIN(timestamp), MAX(timestamp) FROM blocks WHERE block_number BETWEEN 17000000 AND 17000030")
block_min, block_max = cur.fetchone()
print(f"\nCEX range: {cex_min} – {cex_max} ({cex_count} rows)")
print(f"Block ts range: {block_min} – {block_max}")
if cex_min and block_min:
    print(f"CEX covers block range: {'✅ YES' if cex_min <= block_min and cex_max >= block_max else '❌ NO'}")

conn.close()

print("\n" + "=" * 70)
print("DIAGNOSTIC 6: V2 vs V3 volume ratio at block 17M")
print("=" * 70)

# Count V2 vs V3 swap events for ALL pools in a single block
for bn in [17000000, 17000005]:
    v2_count = len(get_logs(bn, bn, [V2_SWAP_TOPIC]))
    time.sleep(0.3)
    v3_count = len(get_logs(bn, bn, [V3_SWAP_TOPIC]))
    time.sleep(0.3)
    total = v2_count + v3_count
    v3_pct = (v3_count / total * 100) if total > 0 else 0
    print(f"Block {bn}: V2 swaps={v2_count}, V3 swaps={v3_count}, V3 share={v3_pct:.1f}%")

print("\n" + "=" * 70)
print("DIAGNOSTIC 7: Check for known MEV patterns in block 17000000")
print("=" * 70)

# Look for common MEV contract patterns
# Check if any txs go to known MEV bot addresses or have flashbots-style coinbase transfers
block_data = rpc_call("eth_getBlockByNumber", [hex(17000000), True])
if block_data:
    txs = block_data.get("transactions", [])
    miner = block_data.get("miner", "")
    print(f"Block 17000000: {len(txs)} txs, miner/builder: {miner}")

    # Check for coinbase transfers (common MEV payment pattern)
    coinbase_txs = [tx for tx in txs if tx.get("to", "").lower() == miner.lower() and hex_to_int(tx.get("value", "0x0")) > 0]
    print(f"Direct payments to builder: {len(coinbase_txs)}")
    for tx in coinbase_txs[:3]:
        val_eth = hex_to_int(tx["value"]) / 1e18
        print(f"  TX {tx['hash'][:18]}... value={val_eth:.6f} ETH from {tx['from'][:12]}...")

    # Check for sandwich patterns (tx to same contract appearing multiple times)
    to_addrs = [tx.get("to", "").lower() for tx in txs if tx.get("to")]
    from collections import Counter
    addr_counts = Counter(to_addrs)
    repeated = {addr: cnt for addr, cnt in addr_counts.items() if cnt >= 3 and addr != miner.lower()}
    if repeated:
        print(f"Contracts called 3+ times (potential sandwich/arb bots): {len(repeated)}")
        for addr, cnt in list(repeated.items())[:5]:
            print(f"  {addr}: {cnt} calls")

print("\n" + "=" * 70)
print("SUMMARY")
print("=" * 70)
