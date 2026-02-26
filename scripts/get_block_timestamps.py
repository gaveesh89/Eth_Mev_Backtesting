#!/usr/bin/env python3
"""Get block timestamps for the depeg window."""
import os, requests, datetime

url = os.environ.get("MEV_RPC_URL", "")
if not url:
    print("MEV_RPC_URL not set")
    exit(1)

for blk in [16817000, 16817050, 16817099]:
    r = requests.post(url, json={"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[hex(blk), False]})
    ts = int(r.json()["result"]["timestamp"], 16)
    dt = datetime.datetime.utcfromtimestamp(ts)
    print(f"Block {blk}: timestamp={ts}  UTC={dt}")
