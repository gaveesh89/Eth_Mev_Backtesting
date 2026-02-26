#!/usr/bin/env python3
"""Focused scan for cross-DEX arb transactions in known-productive block ranges."""
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
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    result = data.get("result", [])
    if isinstance(result, str):
        print(f"  API message: {result}")
        return []
    return result

def find_cross_dex(logs):
    by_tx = defaultdict(list)
    for log in logs:
        bn = int(log["blockNumber"], 16)
        tx = log["transactionHash"].lower()
        addr = log["address"].lower()
        name = TRACKED.get(addr)
        if name:
            by_tx[(bn, tx)].append((addr, name))

    results = []
    for (bn, tx), entries in sorted(by_tx.items()):
        addrs = set(a for a, _ in entries)
        names = [n for _, n in entries]
        for pa, pb, pair_name in ARB_PAIRS:
            if pa in addrs and pb in addrs:
                results.append({
                    "block_number": bn,
                    "tx_hash": tx,
                    "pair": f"{pa}/{pb}",
                    "pair_name": pair_name,
                    "pools": names,
                })
    return results

# Focused ranges: Merge blocks + post-Merge
ranges = [
    (15537393, 15537410, "Merge era (our fixture blocks)"),
    (15537410, 15537450, "Merge +10-50"),
    (15537450, 15537500, "Merge +50-100"),
    (15537500, 15537550, "Merge +100-150"),
    (15537550, 15537600, "Merge +150-200"),
    (15537600, 15537700, "Merge +200-300"),
    (15537700, 15537800, "Merge +300-400"),
    (15537800, 15537900, "Merge +400-500"),
    (15537900, 15538000, "Merge +500-600"),
    (15538000, 15538100, "Merge +600-700"),
]

all_found = []
for from_b, to_b, label in ranges:
    time.sleep(0.3)
    try:
        logs = fetch_logs(from_b, to_b)
    except Exception as e:
        print(f"  Error fetching {label}: {e}")
        continue
    found = find_cross_dex(logs)
    if found:
        for f in found:
            print(f"  CROSS-DEX: block={f['block_number']} tx={f['tx_hash']} pair={f['pair_name']} pools={f['pools']}")
            all_found.append(f)
    else:
        print(f"  {label}: {len(logs)} logs, 0 cross-DEX arbs")

print(f"\n=== TOTAL: {len(all_found)} cross-DEX arb transactions ===")
for f in all_found:
    print(json.dumps(f, indent=2))
