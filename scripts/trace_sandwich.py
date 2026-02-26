#!/usr/bin/env python3
"""Trace MEV sandwich patterns in block 17000000."""
import requests, os, time

RPC = os.environ["MEV_RPC_URL"]
V2_SWAP = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"

def rpc(method, params):
    return requests.post(RPC, json={"jsonrpc":"2.0","id":1,"method":method,"params":params}).json().get("result")

block = rpc("eth_getBlockByNumber", [hex(17000000), True])
txs = block["transactions"]
print(f"Block 17000000: {len(txs)} txs, builder: {block['miner'][:18]}...")
print()

for i in range(min(8, len(txs))):
    tx = txs[i]
    receipt = rpc("eth_getTransactionReceipt", [tx["hash"]])
    logs = receipt.get("logs", [])
    swaps = [l for l in logs if l.get("topics") and l["topics"][0] == V2_SWAP]
    gas = int(receipt["gasUsed"], 16)
    value = int(tx.get("value", "0x0"), 16) / 1e18

    pools = [s["address"][:14] + "..." for s in swaps]
    print(f"TX #{i}: {tx['hash'][:18]}...")
    print(f"  from={tx['from'][:14]}... to={(tx.get('to') or 'create')[:14]}...")
    print(f"  gas={gas:,} value={value:.6f} ETH")
    print(f"  swaps: {len(swaps)} on {pools}")

    if i > 0 and tx["from"].lower() == txs[0]["from"].lower():
        print(f"  *** SAME SENDER as TX #0 — likely sandwich ***")
    print()
    time.sleep(0.3)
