#!/usr/bin/env python3
"""Get early txs from blocks with spread > 10 bps, plus verify the spread
for blocks where we found multi-DEX swaps at low tx indices."""
import subprocess, json, os, sys

rpc_url = os.environ.get("MEV_RPC_URL")
if not rpc_url:
    print("MEV_RPC_URL not set"); sys.exit(1)

def rpc(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params})
    result = subprocess.run(["curl","-s","-X","POST","-H","Content-Type: application/json","-d",payload,rpc_url],
                          capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout).get("result")

def get_reserves(pair, block_hex):
    result = rpc("eth_call", [{"to":pair,"data":"0x0902f1ac"}, block_hex])
    if not result or len(result) < 130:
        return None, None
    r = result[2:]
    return int(r[0:64],16), int(r[64:128],16)

UNI = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
SUSHI = "0x397FF1542f962076d0bFe58EA045FfA2d347aca0"

# Blocks to check: 
# - 16817040, 16817045 (confirmed > 10 bps from first run)
# - 16817076, 16817097 (had V2 swaps at tx_idx 1 and 2 respectively)
# - Also check a wider range: blocks 16817050, 16817060, 16817070, 16817080, 16817090
blocks_to_check = [16817040, 16817045, 16817050, 16817060, 16817070, 16817076, 16817080, 16817090, 16817097]

print("=== Spread + Early TX Analysis ===\n")

good_fixture_candidates = []

for block in blocks_to_check:
    state = hex(block - 1)
    ur0, ur1 = get_reserves(UNI, state)
    sr0, sr1 = get_reserves(SUSHI, state)
    
    if ur0 is None or sr0 is None or ur1 == 0 or sr1 == 0:
        print(f"block {block}: reserve read error")
        continue
    
    uni_price = ur0 * 1e12 / ur1
    sushi_price = sr0 * 1e12 / sr1
    spread_bps = abs(uni_price - sushi_price) / ((uni_price + sushi_price)/2) * 10000
    
    print(f"block={block} uni=${uni_price:.2f} sushi=${sushi_price:.2f} spread={spread_bps:.2f}bps", end="")
    if spread_bps > 10:
        print(" *** ABOVE THRESHOLD ***")
    else:
        print()
    
    # Get first 4 txs
    block_data = rpc("eth_getBlockByNumber", [hex(block), True])
    if block_data:
        base_fee_hex = block_data.get("baseFeePerGas", "0x0")
        base_fee = int(base_fee_hex, 16)
        txs = block_data.get("transactions", [])
        print(f"  base_fee={base_fee} ({base_fee/1e9:.2f} gwei), total_txs={len(txs)}")
        for i, tx in enumerate(txs[:4]):
            gas = int(tx.get("gas","0x0"), 16)
            print(f"  tx_idx={i} hash={tx['hash']} from={tx['from'][:14]}... gas={gas}")
        
        if spread_bps > 10 and len(txs) > 0:
            for i in range(min(4, len(txs))):
                good_fixture_candidates.append({
                    "block_number": block,
                    "tx_hash": txs[i]["hash"],
                    "tx_index": i,
                    "spread_bps": round(spread_bps, 2),
                    "base_fee_gwei": round(base_fee/1e9, 2),
                })
    print()

if good_fixture_candidates:
    print("=== FIXTURE CANDIDATES (spread > 10 bps, tx_index <= 3) ===")
    for c in good_fixture_candidates:
        print(f"  block={c['block_number']} tx_idx={c['tx_index']} spread={c['spread_bps']}bps base_fee={c['base_fee_gwei']}gwei tx={c['tx_hash']}")
    
    # Build the pair string (UniV2/Sushi format)
    uni_pair_addr = "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc"
    sushi_pair_addr = "0x397ff1542f962076d0bfe58ea045ffa2d347aca0"
    pair_str = f"{uni_pair_addr}/{sushi_pair_addr}"
    
    print(f"\n  Pair string for fixture: {pair_str}")
    print(f"\n  Total fixture candidates: {len(good_fixture_candidates)}")
