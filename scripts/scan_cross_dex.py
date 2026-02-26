#!/usr/bin/env python3
"""Scan for cross-DEX arb transactions using Etherscan V2 getLogs API."""
import json, os, sys, time, urllib.request
from collections import defaultdict

API_KEY = os.environ.get("ETHERSCAN_API_KEY", "")
if not API_KEY:
    print("Set ETHERSCAN_API_KEY"); sys.exit(1)

SWAP_TOPIC = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
TRACKED = {
    "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc": "UniV2_WETH_USDC",
    "0x397ff1542f962076d0bfe58ea045ffa2d347aca0": "Sushi_WETH_USDC",
    "0xa478c2975ab1ea89e8196811f51a7b7ade33eb11": "UniV2_WETH_DAI",
    "0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f": "Sushi_WETH_DAI",
    "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852": "UniV2_WETH_USDT",
    "0x06da0fd433c1a5d7a4faa01111c044910a184553": "Sushi_WETH_USDT",
}
ARB_PAIRS = [
    ("0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc", "0x397ff1542f962076d0bfe58ea045ffa2d347aca0", "WETH/USDC"),
    ("0xa478c2975ab1ea89e8196811f51a7b7ade33eb11", "0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f", "WETH/DAI"),
    ("0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852", "0x06da0fd433c1a5d7a4faa01111c044910a184553", "WETH/USDT"),
]

def fetch_logs(from_block, to_block):
    url = (
        f"https://api.etherscan.io/v2/api?chainid=1&module=logs&action=getLogs"
        f"&fromBlock={from_block}&toBlock={to_block}"
        f"&topic0={SWAP_TOPIC}&apikey={API_KEY}"
    )
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read())
    result = data.get("result", [])
    if isinstance(result, str):
        print(f"  API message: {result}")
        return []
    return result

# Scan multiple block ranges
ranges = [
    (15537390, 15537420, "The Merge (Sep 15 2022)"),
    (15537500, 15537600, "Merge +100 blocks"),
    (16817000, 16817050, "USDC depeg (Mar 2023)"),
    (16803100, 16803200, "USDC depeg peak"),
    (18000000, 18000050, "Normal activity (Oct 2023)"),
]

all_cross_dex = []

for from_b, to_b, label in ranges:
    print(f"\n=== {label}: blocks {from_b}-{to_b} ===")
    time.sleep(0.25)
    logs = fetch_logs(from_b, to_b)
    print(f"  Total V2 Swap logs: {len(logs)}")

    by_block_tx = defaultdict(list)
    for log in logs:
        bn = int(log["blockNumber"], 16)
        tx = log["transactionHash"].lower()
        addr = log["address"].lower()
        name = TRACKED.get(addr, None)
        by_block_tx[(bn, tx)].append((addr, name))

    for (bn, tx), entries in sorted(by_block_tx.items()):
        tracked_addrs = set()
        tracked_names = []
        for addr, name in entries:
            if name:
                tracked_addrs.add(addr)
                tracked_names.append(name)

        if len(tracked_addrs) >= 2:
            # Check which arb pairs are covered
            for pa, pb, pair_name in ARB_PAIRS:
                if pa in tracked_addrs and pb in tracked_addrs:
                    print(f"  *** CROSS-DEX ARB *** block={bn} tx={tx[:18]}... pair={pair_name} pools={tracked_names}")
                    all_cross_dex.append({
                        "block_number": bn,
                        "tx_hash": tx,
                        "pair": f"{pa}/{pb}",
                        "pair_name": pair_name,
                        "pools": tracked_names,
                    })
        elif tracked_names:
            pass  # single-pool swap, skip

print(f"\n=== TOTAL CROSS-DEX ARB TRANSACTIONS FOUND: {len(all_cross_dex)} ===")
for entry in all_cross_dex:
    print(f"  block={entry['block_number']} tx={entry['tx_hash'][:20]}... pair={entry['pair_name']}")
    print(f"    url=https://etherscan.io/tx/{entry['tx_hash']}#eventlog")
