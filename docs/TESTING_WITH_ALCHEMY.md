# Testing With Alchemy (Project-Ready)

This guide matches the current CLI implementation in this repo.

## 1) Configure environment

```bash
cp .env.example .env
# edit .env with your Alchemy key
source .env
```

Required variable:
- `MEV_RPC_URL`

Optional:
- `MEV_DB_PATH` (defaults to `data/mev.sqlite` in scripts)
- `RUST_LOG`

## 2) Quick sanity test (small range)

```bash
bash scripts/test_fetch_quick.sh 21500000 21500005
```

Equivalent direct command:

```bash
cargo run -p mev-cli -- \
  --db-path "${MEV_DB_PATH:-data/mev.sqlite}" \
  fetch \
  --start-block 21500000 \
  --end-block 21500005
```

## 3) Larger benchmark run

```bash
bash scripts/test_fetch_benchmark.sh 21500000 21500100
```

## 4) Validate stored data

```bash
sqlite3 "${MEV_DB_PATH:-data/mev.sqlite}" < sql/validate_blocks.sql
```

Checks included:
- block count and tx count
- per-block `transaction_count` vs stored tx rows
- per-block `gas_used` vs `SUM(block_transactions.gas_used)`
- sample receipt-derived tx fields

## 5) Useful troubleshooting

- If command says `MEV_RPC_URL is required`: run `source .env` in the same shell.
- If fetch is slow/timeouts: rerun with smaller ranges first.
- If DB is empty: verify command used `fetch` subcommand and valid range.

## 6) Next test after fetch

Run simulation for a fetched block:

```bash
cargo run -p mev-cli -- \
  --db-path "${MEV_DB_PATH:-data/mev.sqlite}" \
  simulate \
  --block 21500000 \
  --algorithm egp
```
