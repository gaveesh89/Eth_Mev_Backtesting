#!/usr/bin/env python3
"""Quick spread check using subprocess + curl to avoid Python SSL issues."""
import subprocess, json, os, sys

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

UNI_PAIR = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI_PAIR = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"
GET_RESERVES = "0x0902f1ac"

def eth_call(to, block_hex):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_call","params":[{"to":to,"data":GET_RESERVES},block_hex]})
    result = subprocess.run(["curl","-s","-X","POST","-H","Content-Type: application/json","-d",payload,rpc_url], capture_output=True, text=True, timeout=30)
    data = json.loads(result.stdout)
    r = data.get("result","")[2:]
    if len(r) >= 128:
        return int(r[0:64],16), int(r[64:128],16)
    return None, None

# Check every 5th block (state is block-1)
blocks = list(range(16817000, 16817100, 5))
good = []

print(f"{'block':>10} | {'uni_price':>12} | {'sushi_price':>12} | {'spread_bps':>10} | profitable?")
print("-" * 70)

for block in blocks:
    state_block = hex(block - 1)
    ur0, ur1 = eth_call(UNI_PAIR, state_block)
    sr0, sr1 = eth_call(SUSHI_PAIR, state_block)
    
    if ur0 is None or sr0 is None or ur1 == 0 or sr1 == 0:
        print(f"{block:>10} | ERROR")
        continue
    
    # token0=USDC(6dec), token1=WETH(18dec)
    # price in USDC per WETH = reserve0 * 1e12 / reserve1
    uni_price = ur0 * 1e12 / ur1
    sushi_price = sr0 * 1e12 / sr1
    spread = abs(uni_price - sushi_price)
    mid = (uni_price + sushi_price) / 2
    spread_bps = (spread / mid) * 10000
    
    ok = "YES" if spread_bps > 10 else "no"
    print(f"{block:>10} | {uni_price:>12.2f} | {sushi_price:>12.2f} | {spread_bps:>10.2f} | {ok}")
    
    if spread_bps > 10:
        good.append((block, uni_price, sushi_price, spread_bps))

print(f"\nBlocks with spread > 10 bps: {len(good)}/{len(blocks)}")
for b, up, sp, sbps in sorted(good, key=lambda x: -x[3])[:10]:
    direction = "buy_uni_sell_sushi" if up < sp else "buy_sushi_sell_uni"
    print(f"  block={b} spread={sbps:.2f}bps direction={direction}")
