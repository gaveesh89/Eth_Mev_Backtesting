-- New 17M dataset
SELECT '17M_blocks' AS metric, count(*) AS value FROM blocks WHERE block_number BETWEEN 17000000 AND 17000030;
SELECT '17M_txs' AS metric, sum(transaction_count) AS value FROM blocks WHERE block_number BETWEEN 17000000 AND 17000030;
SELECT '17M_sim_' || ordering_algorithm AS metric, count(*) AS runs FROM simulation_results WHERE block_number BETWEEN 17000000 AND 17000030 GROUP BY ordering_algorithm;
SELECT '17M_sim_opps_' || ordering_algorithm AS metric, sum(tx_count) AS value FROM simulation_results WHERE block_number BETWEEN 17000000 AND 17000030 GROUP BY ordering_algorithm;
SELECT '17M_intra_arbs' AS metric, count(*) AS value FROM intra_block_arbs WHERE block_number BETWEEN 17000000 AND 17000030;
SELECT '17M_cex_klines' AS metric, count(*) AS value FROM cex_prices WHERE timestamp_s BETWEEN 1680911000 AND 1680913000;
SELECT '17M_total_value_eth' AS metric, round(sum(CAST(total_value_wei AS REAL))/1e18, 6) AS value FROM simulation_results WHERE block_number BETWEEN 17000000 AND 17000030 AND ordering_algorithm='arbitrage';

-- Original dataset (16.8M range)
SELECT '16.8M_blocks' AS metric, count(*) AS value FROM blocks WHERE block_number BETWEEN 16817000 AND 16817100;
SELECT '16.8M_txs' AS metric, sum(transaction_count) AS value FROM blocks WHERE block_number BETWEEN 16817000 AND 16817100;
SELECT '16.8M_sim_' || ordering_algorithm AS metric, count(*) AS runs FROM simulation_results WHERE block_number BETWEEN 16817000 AND 16817100 GROUP BY ordering_algorithm;
SELECT '16.8M_sim_opps_' || ordering_algorithm AS metric, sum(tx_count) AS value FROM simulation_results WHERE block_number BETWEEN 16817000 AND 16817100 GROUP BY ordering_algorithm;
SELECT '16.8M_intra_arbs' AS metric, count(*) AS value FROM intra_block_arbs WHERE block_number BETWEEN 16817000 AND 16817100;
SELECT '16.8M_total_value_eth' AS metric, round(sum(CAST(total_value_wei AS REAL))/1e18, 6) AS value FROM simulation_results WHERE block_number BETWEEN 16817000 AND 16817100 AND ordering_algorithm='arbitrage';

-- All distinct block ranges
SELECT 'range' AS metric, min(block_number) || '-' || max(block_number) AS value FROM blocks;
SELECT 'total_blocks' AS metric, count(*) AS value FROM blocks;
SELECT 'total_sim_rows' AS metric, count(*) AS value FROM simulation_results;
