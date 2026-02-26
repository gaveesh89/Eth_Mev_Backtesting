#!/usr/bin/env python3
"""Write the verified fixture file."""
import json, os

fixtures = [
    {
        "block_number": 15537405,
        "tx_hash": "0xadc20e6f0eaed93b905f8775c4bcaa16d33dd4ce4f842384fa252768bc7e4713",
        "tx_index": 0,
        "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0xadc20e6f0eaed93b905f8775c4bcaa16d33dd4ce4f842384fa252768bc7e4713#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15537582,
        "tx_hash": "0xd2e44814dac33d5f358e61f9acefdb697ceb907e88e515c3d3b97ac782402b84",
        "tx_index": 0,
        "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0xd2e44814dac33d5f358e61f9acefdb697ceb907e88e515c3d3b97ac782402b84#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15537616,
        "tx_hash": "0x3828c823249e1413b0f82340a307a92bf64a17ae97f4103556747101234fbfc4",
        "tx_index": 0,
        "pair": "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852/0x06da0fd433c1a5d7a4faa01111c044910a184553",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0x3828c823249e1413b0f82340a307a92bf64a17ae97f4103556747101234fbfc4#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15537913,
        "tx_hash": "0x9b08cae9e6baec3122083d2c25a9a38ccca6bc12fc626ce122ff602e8af23c74",
        "tx_index": 0,
        "pair": "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc/0x397ff1542f962076d0bfe58ea045ffa2d347aca0",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0x9b08cae9e6baec3122083d2c25a9a38ccca6bc12fc626ce122ff602e8af23c74#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15538813,
        "tx_hash": "0x03fd882ddb294a81f285bfc87ebfc47a02a2eb20d25813de666ffaed12f529e4",
        "tx_index": 0,
        "pair": "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc/0x397ff1542f962076d0bfe58ea045ffa2d347aca0",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0x03fd882ddb294a81f285bfc87ebfc47a02a2eb20d25813de666ffaed12f529e4#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15538813,
        "tx_hash": "0x03fd882ddb294a81f285bfc87ebfc47a02a2eb20d25813de666ffaed12f529e4",
        "tx_index": 0,
        "pair": "0xa478c2975ab1ea89e8196811f51a7b7ade33eb11/0xc3d03e4f041fd4cd388c549ee2a29a9e5075882f",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0x03fd882ddb294a81f285bfc87ebfc47a02a2eb20d25813de666ffaed12f529e4#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
    {
        "block_number": 15539280,
        "tx_hash": "0xbcd7296c3a154a4e1f3c78b466435131c8b6219090d44dcb0cd2759bdeb09a71",
        "tx_index": 0,
        "pair": "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc/0x397ff1542f962076d0bfe58ea045ffa2d347aca0",
        "profit_approx_wei": 0,
        "verification_url": "https://etherscan.io/tx/0xbcd7296c3a154a4e1f3c78b466435131c8b6219090d44dcb0cd2759bdeb09a71#eventlog",
        "verification_method": "etherscan_getLogs_block_swap_scan",
        "verification_status": "CONFIRMED_CROSS_DEX_ARB",
    },
]

path = os.path.join(os.path.dirname(__file__), "..", "test_data", "known_arb_txs.json")
with open(path, "w") as f:
    json.dump(fixtures, f, indent=2)
    f.write("\n")
print(f"Wrote {len(fixtures)} fixtures to {os.path.abspath(path)}")
