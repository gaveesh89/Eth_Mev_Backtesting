#!/usr/bin/env python3
"""Get full tx hashes for specific blocks."""
import json, os, time, urllib.request
from collections import defaultdict

API_KEY = os.environ.get("ETHERSCAN_API_KEY", "")
SWAP = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
TRACKED = {
    "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc": "UniV2_USDC",
    "0x397ff1542f962076d0bfe58ea045ffa2d347aca0": "Sushi_USDC",
    "0xa478c2975ab1ea89e8196811f51a7b7ade33eb11": "UniV2_DAI",
    "0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f": "Sushi_DAI",
    "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852": "UniV2_USDT",
    "0x06da0fd433c1a5d7a4faa01111c044910a184553": "Sushi_USDT",
}
PAIRS = {
    frozenset(["0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc","0x397ff1542f962076d0bfe58ea045ffa2d347aca0"]): ("WETH/USDC", "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc/0x397ff1542f962076d0bfe58ea045ffa2d347aca0"),
    frozenset(["0xa478c2975ab1ea89e8196811f51a7b7ade33eb11","0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f"]): ("WETH/DAI", "0xa478c2975ab1ea89e8196811f51a7b7ade33eb11/0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f"),
    frozenset(["0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852","0x06da0fd433c1a5d7a4faa01111c044910a184553"]): ("WETH/USDT", "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553"),
}

blocks = [15538813, 15539280]

for bn in blocks:
    time.sleep(0.3)
    url = f"https://api.etherscan.io/v2/api?chainid=1&module=logs&action=getLogs&fromBlock={bn}&toBlock={bn}&topic0={SWAP}&apikey={API_KEY}"
    with urllib.request.urlopen(url, timeout=30) as r:
        data = json.loads(r.read())
    by_tx = defaultdict(set)
    for log in data["result"]:
        tx = log["transactionHash"].lower()
        addr = log["address"].lower()
        if addr in TRACKED:
            by_tx[tx].add(addr)
    for tx, addrs in by_tx.items():
        for pair_set, (pair_name, pair_str) in PAIRS.items():
            if pair_set.issubset(addrs):
                print(f"block={bn} pair={pair_name} pair_str={pair_str}")
                print(f"  tx_hash={tx}")
                print(f"  url=https://etherscan.io/tx/{tx}#eventlog")
