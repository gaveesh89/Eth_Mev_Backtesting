#!/usr/bin/env python3
"""Wider scan for cross-DEX arb transactions - scan more blocks."""
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
                results.append({"block_number": bn, "tx_hash": tx, "pair": f"{pa}/{pb}", "pair_name": pair_name, "pools": names})
    return results

# Extended merge-era scan plus some high-activity blocks
ranges = [
    (15538000, 15538200, "Merge +600-800"),
    (15538200, 15538400, "Merge +800-1000"),
    (15538400, 15538600, "Merge +1000-1200"),
    (15538600, 15538800, "Merge +1200-1400"),
    (15538800, 15539000, "Merge +1400-1600"),
    (15539000, 15539200, "Merge +1600-1800"),
    (15539200, 15539400, "Merge +1800-2000"),
]

all_found = []
for from_b, to_b, label in ranges:
    time.sleep(0.3)
    try:
        logs = fetch_logs(from_b, to_b)
    except Exception as e:
        print(f"  Error {label}: {e}")
        continue
    found = find_cross_dex(logs)
    for f in found:
        print(f"  CROSS-DEX: block={f['block_number']} tx={f['tx_hash'][:20]}... pair={f['pair_name']}")
        all_found.append(f)
    if not found:
        print(f"  {label}: {len(logs)} logs, 0 cross-DEX")

print(f"\n=== TOTAL NEW: {len(all_found)} ===")
# Combined with previous 4
prev = [
    {"block_number": 15537405, "tx_hash": "0xadc20e6f0eaed93b905f8775c4bcaa16d33dd4ce4f842384fa252768bc7e4713", "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553", "pair_name": "WETH/USDT"},
    {"block_number": 15537582, "tx_hash": "0xd2e44814dac33d5f358e61f9acefdb697ceb907e88e515c3d3b97ac782402b84", "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553", "pair_name": "WETH/USDT"},
    {"block_number": 15537616, "tx_hash": "0x3828c823249e1413b0f82340a307a92bf64a17ae97f4103556747101234fbfc4", "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553", "pair_name": "WETH/USDT"},
    {"block_number": 15537913, "tx_hash": "0x9b08cae9e6baec3122083d2c25a9a38ccca6bc12fc626ce122ff602e8af23c74", "pair": "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc/0x397ff1542f962076d0bfe58ea045ffa2d347aca0", "pair_name": "WETH/USDC"},
]
combined = prev + all_found
print(f"\n=== ALL COMBINED: {len(combined)} cross-DEX arbs ===")
for f in combined:
    print(f"  block={f['block_number']} pair={f['pair_name']} tx={f['tx_hash'][:20]}...")
