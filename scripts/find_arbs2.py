#!/usr/bin/env python3
"""Scan blocks 16817000-16817099 for all Swap events,
identify transactions touching our tracked pools,
and find cross-pool arb candidates."""

import urllib.request
import json
import os
import sys
import time

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

UNI_V2_WETH_USDC = "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc"
SUSHI_WETH_USDC  = "0x397ff1542f962076d0bfe58ea045ffa2d347aca0"
TRACKED = {UNI_V2_WETH_USDC, SUSHI_WETH_USDC}

SWAP_TOPIC = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"

def rpc(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(rpc_url, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read()).get("result")

# Phase 1: Get all V2 Swap logs per block (10-block chunks to avoid 400)
print("=== Phase 1: Gathering Swap events ===")
all_logs = []
for start in range(16817000, 16817100, 10):
    end = start + 9
    logs = rpc("eth_getLogs", [{"fromBlock": hex(start), "toBlock": hex(end), "topics": [SWAP_TOPIC]}])
    if logs:
        all_logs.extend(logs)
    print(f"  blocks {start}-{end}: {len(logs or [])} swaps")
    time.sleep(0.2)

print(f"\nTotal V2 Swap events: {len(all_logs)}")

# Phase 2: Group by tx_hash -> list of swap pool addresses
tx_swaps = {}
for log in all_logs:
    tx = log["transactionHash"]
    pool = log["address"].lower()
    block = int(log["blockNumber"], 16)
    tx_idx = int(log["transactionIndex"], 16)
    if tx not in tx_swaps:
        tx_swaps[tx] = {"block": block, "tx_idx": tx_idx, "pools": []}
    tx_swaps[tx]["pools"].append(pool)

print(f"Unique transactions with V2 swaps: {len(tx_swaps)}")

# Phase 3: Find arb candidates — txs with 2+ pool swaps where at least one is tracked
print("\n=== Phase 2: Identifying arb candidates ===")
arb_candidates = []
for tx, info in sorted(tx_swaps.items(), key=lambda x: (x[1]["block"], x[1]["tx_idx"])):
    pool_set = set(info["pools"])
    tracked_hit = pool_set & TRACKED
    
    if len(pool_set) >= 2 and len(tracked_hit) >= 1:
        arb_candidates.append({
            "tx": tx,
            "block": info["block"],
            "tx_idx": info["tx_idx"],
            "pools": pool_set,
            "tracked": tracked_hit,
            "num_swaps": len(info["pools"]),
        })

print(f"Arb candidates (multi-pool, touching tracked): {len(arb_candidates)}")

for c in arb_candidates:
    tracked_names = []
    if UNI_V2_WETH_USDC in c["tracked"]:
        tracked_names.append("UniV2")
    if SUSHI_WETH_USDC in c["tracked"]:
        tracked_names.append("Sushi")
    other = c["pools"] - TRACKED
    print(f"  block={c['block']} tx_idx={c['tx_idx']:3d} swaps={c['num_swaps']} tracked=[{','.join(tracked_names)}] other_pools={len(other)} tx={c['tx']}")

# Phase 4: Cross-DEX (both tracked pools in same tx)
cross_dex = [c for c in arb_candidates if len(c["tracked"]) == 2]
print(f"\nCross-DEX (both Uni+Sushi): {len(cross_dex)}")
for c in cross_dex:
    print(f"  block={c['block']} tx_idx={c['tx_idx']:3d} tx={c['tx']}")

# Phase 5: Get receipts for top arb candidates to check gas and profit hints
print("\n=== Phase 3: Receipt details for arb candidates ===")
for c in arb_candidates[:20]:
    receipt = rpc("eth_getTransactionReceipt", [c["tx"]])
    if not receipt:
        continue
    gas = int(receipt["gasUsed"], 16)
    status = int(receipt["status"], 16)
    tx_data = rpc("eth_getTransactionByHash", [c["tx"]])
    from_addr = tx_data["from"] if tx_data else "?"
    to_addr = tx_data["to"] if tx_data else "?"
    print(f"  block={c['block']} tx_idx={c['tx_idx']:3d} gas={gas:,} status={'OK' if status else 'FAIL'} from={from_addr} to={to_addr} tx={c['tx']}")
    time.sleep(0.1)
