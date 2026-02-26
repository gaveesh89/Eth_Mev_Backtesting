#!/usr/bin/env python3
"""Find atomic arbitrage transactions in blocks 16817000-16817099
that touch our tracked Uniswap V2 and SushiSwap WETH/USDC pools."""

import urllib.request, json, os, sys

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set")
    sys.exit(1)

UNI_V2_WETH_USDC = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".lower()
SUSHI_WETH_USDC  = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0".lower()
TRACKED = {UNI_V2_WETH_USDC, SUSHI_WETH_USDC}

# Uniswap V2 Swap event topic
SWAP_TOPIC = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(rpc_url, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())["result"]

# Step 1: Get ALL Swap events on BOTH pools in the block range
print("=== Fetching Swap events on tracked pools (blocks 16817000-16817099) ===")

all_tx_pools = {}  # tx_hash -> set of pool addresses that emitted Swap

for pool_name, pool_addr in [("UniV2_WETH_USDC", UNI_V2_WETH_USDC), ("Sushi_WETH_USDC", SUSHI_WETH_USDC)]:
    logs = rpc_call("eth_getLogs", [{
        "fromBlock": hex(16817000),
        "toBlock": hex(16817099),
        "address": pool_addr,
        "topics": [SWAP_TOPIC]
    }])
    print(f"  {pool_name}: {len(logs)} swap events")
    for log in logs:
        tx = log["transactionHash"]
        block = int(log["blockNumber"], 16)
        tx_idx = int(log["transactionIndex"], 16)
        if tx not in all_tx_pools:
            all_tx_pools[tx] = {"block": block, "tx_idx": tx_idx, "pools": set()}
        all_tx_pools[tx]["pools"].add(pool_addr)

print(f"\nTotal unique txs touching tracked pools: {len(all_tx_pools)}")

# Step 2: Find txs that touch BOTH pools (cross-DEX arb candidates)
cross_dex = {tx: info for tx, info in all_tx_pools.items() if len(info["pools"]) == 2}
print(f"Txs touching BOTH pools (cross-DEX arb candidates): {len(cross_dex)}")

for tx, info in sorted(cross_dex.items(), key=lambda x: (x[1]["block"], x[1]["tx_idx"])):
    print(f"  block={info['block']} tx_idx={info['tx_idx']:3d} tx={tx}")

# Step 3: For txs touching a single pool, check if they also swap on other DEXes
# by looking at ALL Swap events in the same transaction
print(f"\n=== Checking single-pool txs for multi-DEX swaps ===")

single_pool_txs = {tx: info for tx, info in all_tx_pools.items() if len(info["pools"]) == 1}
arb_candidates = []

for tx, info in sorted(single_pool_txs.items(), key=lambda x: (x[1]["block"], x[1]["tx_idx"])):
    # Get receipt to see ALL logs in this tx
    receipt = rpc_call("eth_getTransactionReceipt", [tx])
    if not receipt:
        continue
    
    swap_pools = set()
    for log in receipt.get("logs", []):
        if len(log.get("topics", [])) >= 1 and log["topics"][0] == SWAP_TOPIC:
            swap_pools.add(log["address"].lower())
    
    # Multi-pool swap = arb candidate
    if len(swap_pools) >= 2:
        gas_used = int(receipt["gasUsed"], 16)
        arb_candidates.append({
            "tx": tx,
            "block": info["block"],
            "tx_idx": info["tx_idx"],
            "swap_pools": swap_pools,
            "gas_used": gas_used,
            "tracked_pools": swap_pools & TRACKED,
        })

print(f"Single-pool txs with multi-DEX swaps: {len(arb_candidates)}")
for c in arb_candidates:
    pool_list = ", ".join(sorted(c["swap_pools"]))
    tracked = ", ".join(sorted(c["tracked_pools"]))
    print(f"  block={c['block']} tx_idx={c['tx_idx']:3d} gas={c['gas_used']} tracked=[{tracked}] all_pools=[{pool_list}] tx={c['tx']}")

# Step 4: Combined list
print(f"\n=== ALL ATOMIC ARB CANDIDATES ===")
all_arbs = []

for tx, info in cross_dex.items():
    receipt = rpc_call("eth_getTransactionReceipt", [tx])
    gas_used = int(receipt["gasUsed"], 16) if receipt else 0
    all_arbs.append({"tx": tx, "block": info["block"], "tx_idx": info["tx_idx"], "gas_used": gas_used, "type": "cross-dex(uni+sushi)"})

for c in arb_candidates:
    all_arbs.append({"tx": c["tx"], "block": c["block"], "tx_idx": c["tx_idx"], "gas_used": c["gas_used"], "type": "multi-dex"})

all_arbs.sort(key=lambda x: (x["block"], x["tx_idx"]))
print(f"Total arb candidates: {len(all_arbs)}")
for a in all_arbs:
    print(f"  block={a['block']} tx_idx={a['tx_idx']:3d} gas={a['gas_used']} type={a['type']} tx={a['tx']}")
