#!/usr/bin/env python3
import json,os,urllib.request
from collections import defaultdict
API_KEY=os.environ["ETHERSCAN_API_KEY"]
SWAP="0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
T={"0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc":1,"0x397ff1542f962076d0bfe58ea045ffa2d347aca0":1,"0xa478c2975ab1ea89e8196811f51a7b7ade33eb11":1,"0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f":1,"0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852":1,"0x06da0fd433c1a5d7a4faa01111c044910a184553":1}
bn=15539280
url=f"https://api.etherscan.io/v2/api?chainid=1&module=logs&action=getLogs&fromBlock={bn}&toBlock={bn}&topic0={SWAP}&apikey={API_KEY}"
with urllib.request.urlopen(url,timeout=30) as r:
    data=json.loads(r.read())
d=defaultdict(set)
for l in data["result"]:
    tx=l["transactionHash"].lower()
    addr=l["address"].lower()
    if addr in T:
        d[tx].add(addr)
for tx,addrs in d.items():
    if len(addrs)>=2:
        print(f"tx={tx}")
