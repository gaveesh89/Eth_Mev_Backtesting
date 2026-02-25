-- Validate fetched block + transaction data integrity.
-- Usage:
--   sqlite3 "$MEV_DB_PATH" < sql/validate_blocks.sql

.headers on
.mode column

SELECT COUNT(*) AS block_count FROM blocks;
SELECT COUNT(*) AS tx_count FROM block_transactions;

SELECT
  block_number,
  block_hash,
  transaction_count,
  gas_used,
  base_fee_per_gas
FROM blocks
ORDER BY block_number DESC
LIMIT 5;

-- Compare declared transaction_count vs stored tx rows per block (latest 10 blocks)
SELECT
  b.block_number,
  b.transaction_count AS declared_tx_count,
  COUNT(t.tx_hash) AS stored_tx_count,
  CASE
    WHEN b.transaction_count = COUNT(t.tx_hash) THEN 'OK'
    ELSE 'MISMATCH'
  END AS tx_count_check
FROM blocks b
LEFT JOIN block_transactions t ON b.block_number = t.block_number
GROUP BY b.block_number
ORDER BY b.block_number DESC
LIMIT 10;

-- Compare block gas_used vs sum(receipt gas_used) (latest 10 blocks)
SELECT
  b.block_number,
  b.gas_used AS block_gas_used,
  COALESCE(SUM(t.gas_used), 0) AS sum_tx_gas_used,
  CASE
    WHEN b.gas_used = COALESCE(SUM(t.gas_used), 0) THEN 'OK'
    ELSE 'CHECK'
  END AS gas_check
FROM blocks b
LEFT JOIN block_transactions t ON b.block_number = t.block_number
GROUP BY b.block_number
ORDER BY b.block_number DESC
LIMIT 10;

-- Spot-check tx receipt fields for latest block
SELECT
  block_number,
  tx_hash,
  from_address,
  to_address,
  gas_used,
  effective_gas_price,
  status
FROM block_transactions
WHERE block_number = (SELECT MAX(block_number) FROM block_transactions)
LIMIT 10;
