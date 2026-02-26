#!/usr/bin/env python3
"""For each arb candidate: decode Swap event data to confirm token flow direction
and calculate approximate profit."""

import urllib.request
import json
import os
import sys
import time

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

SWAP_TOPIC = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
WETH = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
USDC = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"

UNI_V2_WETH_USDC = "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc"
SUSHI_WETH_USDC  = "0x397ff1542f962076d0bfe58ea045ffa2d347aca0"

# Top arb candidates from Phase 1
candidates = [
    "0x5915f5017e51cd0a631d75cb3a2bf178e5b77ad1a93a4789d7daf9f36987d63f",
    "0x9a6f114536403c090872dad2be377fb2fc35ce3446b10df9406dd43bb2a85709",
    "0x5af56e50465424249d745def2ea001b42fedef5d8919acc48ee020251b12a54faf",
    "0x3afeb59dd727731fad00e1d7c38527670fa2861fda2c4af4fef840ea7d659356",
    "0xb5487eb92441b6decbe9878abd7a41bf68396cc35a97e8968b627d4a19d4c706",
    "0xedaf134aa3b474551af3945fdd4631190b5d3235b61ebcc44d29d6bc36e8853d",
    "0xd5484bdfcfa8e293c05ac0ddebb878c6ab8d76ae908bab79a9bafdd75b37f853",
    "0x69c1c6a6f572d0f118f25ef57119507e7eef18ef540f2d5a720b7e5cf9bbfe3e",
    "0x451fea6b489ea48116725057790d2dbae9134a5bfb5b7302a5ee5c72079c9522",
    "0x0575baf75e9dca1953b9cec7d58eb5540c9c095ad2a2be0afd8fc3db4f9ab70f",
    "0x6eacf592d9e8267457ed9a2a6fa6dcc911be7edf6382c59b322468c14178dd59",
    "0xb9ced110f20eeed132289d7088de081c861233446b088974533595200481b775",
    "0x51271ff67baa22f32e3ab639cedb0c4b1ca411dc7e27eb70326e0427cbb4cb36",
    "0x3f696c98ced3d83e4fd1062f58eba95a5175a30f21715639f950d559af661e5a",
    "0x5e341b8b1f1f8d7e0e2d6da451f7b9358c1d2f65145b52496ec901bf6dd6e831",
    "0x63a0acb6d63e24aae2f2d5f9b4a120e8c8cd2a6418f00c78dd38ecd91f81590f",
    "0xc441519250c12890ecc7e893f18495c833665bb8a7c75c9b1b31bb9c7bdaee6",  
    "0xacf0118e77d6d69ac56a7b22a22cb43311f22a34afe5eb92a171033b73eada75",
    "0x418e3af5cf551e57743fd4f4efa1b4803581b7daa466311baa2e7f43f7f7a4ad",
    "0xd15239b2497effc7fc5a17859b2714150bcc52af9262fae36c976ec0e20fc0e0",
]

def rpc(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(rpc_url, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read()).get("result")

def decode_swap(log_data_hex):
    """Decode V2 Swap event: amount0In, amount1In, amount0Out, amount1Out"""
    d = log_data_hex[2:]  # strip 0x
    a0in  = int(d[0:64], 16)
    a1in  = int(d[64:128], 16)
    a0out = int(d[128:192], 16)
    a1out = int(d[192:256], 16)
    return a0in, a1in, a0out, a1out

print("=== Detailed arb candidate analysis ===\n")

for tx_hash in candidates:
    receipt = rpc("eth_getTransactionReceipt", [tx_hash])
    if not receipt:
        print(f"  SKIP {tx_hash}: no receipt")
        continue
    
    block = int(receipt["blockNumber"], 16)
    tx_idx = int(receipt["transactionIndex"], 16)
    gas_used = int(receipt["gasUsed"], 16)
    
    tx_data = rpc("eth_getTransactionByHash", [tx_hash])
    from_addr = tx_data["from"].lower() if tx_data else "?"
    to_addr = tx_data["to"].lower() if tx_data else "?"
    
    # Find all Swap events
    swaps = []
    for log in receipt.get("logs", []):
        if len(log.get("topics", [])) >= 3 and log["topics"][0] == SWAP_TOPIC:
            pool = log["address"].lower()
            sender = "0x" + log["topics"][1][26:]
            to = "0x" + log["topics"][2][26:]
            a0in, a1in, a0out, a1out = decode_swap(log["data"])
            swaps.append({"pool": pool, "sender": sender, "to": to, 
                         "a0in": a0in, "a1in": a1in, "a0out": a0out, "a1out": a1out})
    
    # Find WETH and USDC transfers to/from the bot address
    weth_net = 0
    usdc_net = 0
    for log in receipt.get("logs", []):
        if len(log.get("topics", [])) >= 3 and log["topics"][0] == TRANSFER_TOPIC:
            token = log["address"].lower()
            from_t = "0x" + log["topics"][1][26:]
            to_t = "0x" + log["topics"][2][26:]
            amount = int(log["data"], 16)
            
            if token == WETH:
                if to_t.lower() == to_addr:
                    weth_net += amount
                elif from_t.lower() == to_addr:
                    weth_net -= amount
            elif token == USDC:
                if to_t.lower() == to_addr:
                    usdc_net += amount
                elif from_t.lower() == to_addr:
                    usdc_net -= amount
    
    # Determine which tracked pools are involved
    tracked_pools = []
    for s in swaps:
        if s["pool"] == UNI_V2_WETH_USDC:
            tracked_pools.append("UniV2")
        elif s["pool"] == SUSHI_WETH_USDC:
            tracked_pools.append("Sushi")
    
    is_arb = weth_net > 0 or usdc_net > 0  # positive token flow = arb profit
    
    print(f"TX: {tx_hash}")
    print(f"  block={block} tx_idx={tx_idx} gas={gas_used:,}")
    print(f"  from={from_addr} to(contract)={to_addr}")
    print(f"  tracked pools: {tracked_pools}")
    print(f"  swaps: {len(swaps)}")
    for i, s in enumerate(swaps):
        pool_name = "UniV2" if s["pool"] == UNI_V2_WETH_USDC else ("Sushi" if s["pool"] == SUSHI_WETH_USDC else s["pool"][:10])
        print(f"    [{i}] pool={pool_name} a0in={s['a0in']} a1in={s['a1in']} a0out={s['a0out']} a1out={s['a1out']}")
    print(f"  WETH net to bot: {weth_net} ({weth_net/1e18:.6f} ETH)")
    print(f"  USDC net to bot: {usdc_net} ({usdc_net/1e6:.2f} USDC)")
    print(f"  IS ARB: {is_arb}")
    print()
    
    time.sleep(0.15)
