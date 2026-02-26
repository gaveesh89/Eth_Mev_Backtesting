//! SQLite storage layer for mempool transactions and on-chain block data.
//!
//! Uses WAL mode for concurrent read performance and prepared statements
//! for batch insert throughput.
//!
//! ## Why SQLite?
//! rbuilder uses this same pattern for rapid local iteration without
//! requiring a running database server.

use eyre::Result;
use rusqlite::Connection;
use std::cell::RefCell;

use crate::types::{Block, BlockTransaction, MempoolTransaction, TxLog};

/// Row type for intra-block DEX-DEX arbitrage: `(block_number, after_tx_index, after_log_index, pool_a, pool_b, spread_bps, profit_wei, direction, verdict)`.
pub type IntraBlockArbRow = (u64, u64, u64, String, String, i64, String, String, String);

#[allow(dead_code)]
pub struct Store {
    conn: RefCell<Connection>,
}

impl Store {
    /// Creates or opens a SQLite database with WAL mode enabled.
    ///
    /// # Errors
    /// Returns error if the database cannot be opened or migrations fail.
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self {
            conn: RefCell::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.borrow_mut().execute_batch(
            "
            CREATE TABLE IF NOT EXISTS mempool_transactions (
                hash TEXT PRIMARY KEY,
                block_number INTEGER,
                timestamp_ms INTEGER,
                from_address TEXT,
                to_address TEXT,
                value TEXT,
                gas_limit INTEGER,
                gas_price TEXT,
                max_fee_per_gas TEXT,
                max_priority_fee_per_gas TEXT,
                nonce INTEGER,
                input_data TEXT,
                tx_type INTEGER,
                raw_tx TEXT
            );

            CREATE TABLE IF NOT EXISTS blocks (
                block_number INTEGER PRIMARY KEY,
                block_hash TEXT,
                parent_hash TEXT,
                timestamp INTEGER,
                gas_limit INTEGER,
                gas_used INTEGER,
                base_fee_per_gas TEXT,
                miner TEXT,
                transaction_count INTEGER
            );

            CREATE TABLE IF NOT EXISTS block_transactions (
                block_number INTEGER,
                tx_hash TEXT,
                tx_index INTEGER,
                from_address TEXT,
                to_address TEXT,
                gas_used INTEGER,
                effective_gas_price TEXT,
                status INTEGER,
                PRIMARY KEY (block_number, tx_hash)
            );

            CREATE TABLE IF NOT EXISTS simulation_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                block_number INTEGER,
                ordering_algorithm TEXT,
                simulated_at TEXT,
                tx_count INTEGER,
                gas_used INTEGER,
                total_value_wei TEXT,
                mev_captured_wei TEXT
            );

            CREATE TABLE IF NOT EXISTS mev_opportunities (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                simulation_id INTEGER,
                opportunity_type TEXT,
                profit_wei TEXT,
                tx_hashes TEXT,
                protocol TEXT,
                details TEXT
            );

            CREATE TABLE IF NOT EXISTS cex_prices (
                pair TEXT NOT NULL,
                timestamp_s INTEGER NOT NULL,
                open_micro INTEGER NOT NULL,
                close_micro INTEGER NOT NULL,
                high_micro INTEGER NOT NULL,
                low_micro INTEGER NOT NULL,
                PRIMARY KEY (pair, timestamp_s)
            );

            CREATE TABLE IF NOT EXISTS intra_block_arbs (
                block_number INTEGER NOT NULL,
                after_tx_index INTEGER NOT NULL,
                after_log_index INTEGER NOT NULL,
                pool_a TEXT NOT NULL,
                pool_b TEXT NOT NULL,
                spread_bps INTEGER NOT NULL,
                profit_wei TEXT NOT NULL,
                direction TEXT NOT NULL,
                verdict TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (block_number, after_log_index)
            );

            CREATE TABLE IF NOT EXISTS tx_logs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                block_number    INTEGER NOT NULL,
                tx_hash         TEXT NOT NULL,
                tx_index        INTEGER NOT NULL,
                log_index       INTEGER NOT NULL,
                address         TEXT NOT NULL,
                topic0          TEXT NOT NULL,
                topic1          TEXT,
                topic2          TEXT,
                topic3          TEXT,
                data            TEXT NOT NULL DEFAULT '',
                UNIQUE(tx_hash, log_index)
            );
            CREATE INDEX IF NOT EXISTS idx_tx_logs_block ON tx_logs(block_number);
            CREATE INDEX IF NOT EXISTS idx_tx_logs_topic0 ON tx_logs(topic0);
            CREATE INDEX IF NOT EXISTS idx_tx_logs_tx ON tx_logs(tx_hash);
            ",
        )?;
        Ok(())
    }

    /// Batch insert transaction receipt logs.
    ///
    /// Logs with duplicate `(tx_hash, log_index)` are ignored.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_tx_logs(&self, logs: &[TxLog]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "
                INSERT OR IGNORE INTO tx_logs (
                    block_number, tx_hash, tx_index, log_index, address,
                    topic0, topic1, topic2, topic3, data
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;

            for log in logs {
                let affected = stmt.execute(rusqlite::params![
                    log.block_number as i64,
                    log.tx_hash,
                    log.tx_index as i64,
                    log.log_index as i64,
                    log.address,
                    log.topic0,
                    log.topic1,
                    log.topic2,
                    log.topic3,
                    log.data,
                ])?;
                inserted = inserted.saturating_add(affected);
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Retrieve all logs for a given transaction hash.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_logs_for_tx(&self, tx_hash: &str) -> Result<Vec<TxLog>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT block_number, tx_hash, tx_index, log_index, address,
                   topic0, topic1, topic2, topic3, data
            FROM tx_logs
            WHERE tx_hash = ?
            ORDER BY log_index ASC
            ",
        )?;

        let logs = stmt
            .query_map(rusqlite::params![tx_hash], |row| {
                Ok(TxLog {
                    block_number: row.get::<_, i64>(0)? as u64,
                    tx_hash: row.get(1)?,
                    tx_index: row.get::<_, i64>(2)? as u64,
                    log_index: row.get::<_, i64>(3)? as u64,
                    address: row.get(4)?,
                    topic0: row.get(5)?,
                    topic1: row.get(6)?,
                    topic2: row.get(7)?,
                    topic3: row.get(8)?,
                    data: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(logs)
    }

    /// Retrieve all logs for a given block number.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_logs_for_block(&self, block_number: u64) -> Result<Vec<TxLog>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT block_number, tx_hash, tx_index, log_index, address,
                   topic0, topic1, topic2, topic3, data
            FROM tx_logs
            WHERE block_number = ?
            ORDER BY log_index ASC
            ",
        )?;

        let logs = stmt
            .query_map(rusqlite::params![block_number as i64], |row| {
                Ok(TxLog {
                    block_number: row.get::<_, i64>(0)? as u64,
                    tx_hash: row.get(1)?,
                    tx_index: row.get::<_, i64>(2)? as u64,
                    log_index: row.get::<_, i64>(3)? as u64,
                    address: row.get(4)?,
                    topic0: row.get(5)?,
                    topic1: row.get(6)?,
                    topic2: row.get(7)?,
                    topic3: row.get(8)?,
                    data: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(logs)
    }

    /// Retrieve only ERC-20 Transfer logs for a given block number.
    ///
    /// Filters to `topic0 = keccak256("Transfer(address,address,uint256)")`.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_transfer_logs_for_block(&self, block_number: u64) -> Result<Vec<TxLog>> {
        const TRANSFER_TOPIC0: &str =
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT block_number, tx_hash, tx_index, log_index, address,
                   topic0, topic1, topic2, topic3, data
            FROM tx_logs
            WHERE block_number = ? AND topic0 = ?
            ORDER BY log_index ASC
            ",
        )?;

        let logs = stmt
            .query_map(
                rusqlite::params![block_number as i64, TRANSFER_TOPIC0],
                |row| {
                    Ok(TxLog {
                        block_number: row.get::<_, i64>(0)? as u64,
                        tx_hash: row.get(1)?,
                        tx_index: row.get::<_, i64>(2)? as u64,
                        log_index: row.get::<_, i64>(3)? as u64,
                        address: row.get(4)?,
                        topic0: row.get(5)?,
                        topic1: row.get(6)?,
                        topic2: row.get(7)?,
                        topic3: row.get(8)?,
                        data: row.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(logs)
    }

    /// Inserts Binance-derived CEX prices keyed by pair + second timestamp.
    ///
    /// Prices are stored as integers in micro-USD (price × 1e6).
    /// Existing rows are replaced for the same (pair, timestamp).
    ///
    /// Each row: `(pair, timestamp_s, open_micro, close_micro, high_micro, low_micro)`.
    ///
    /// # Errors
    /// Returns error if any database operation fails.
    pub fn insert_cex_prices(&self, rows: &[(&str, u64, i64, i64, i64, i64)]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "
                INSERT OR REPLACE INTO cex_prices (pair, timestamp_s, open_micro, close_micro, high_micro, low_micro)
                VALUES (?, ?, ?, ?, ?, ?)
                ",
            )?;

            for row in rows {
                let affected = stmt.execute(rusqlite::params![
                    row.0,
                    row.1 as i64,
                    row.2,
                    row.3,
                    row.4,
                    row.5,
                ])?;
                inserted = inserted.saturating_add(affected);
            }
        }

        tx.commit()?;
        Ok(inserted)
    }

    /// Returns nearest CEX close price in micro-USD for a given pair and block timestamp.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_nearest_cex_close_price_micro(
        &self,
        pair: &str,
        timestamp_s: u64,
    ) -> Result<Option<(u64, i64)>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT timestamp_s, close_micro
            FROM cex_prices
            WHERE pair = ?
            ORDER BY ABS(timestamp_s - ?) ASC
            LIMIT 1
            ",
        )?;

        let result = stmt.query_row(rusqlite::params![pair, timestamp_s as i64], |row| {
            let candle_timestamp: i64 = row.get(0)?;
            let close_micro: i64 = row.get(1)?;
            Ok((candle_timestamp as u64, close_micro))
        });

        match result {
            Ok(point) => Ok(Some(point)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Stores intra-block DEX-DEX arbitrage opportunities for a block scan.
    ///
    /// Each row is `(block_number, after_tx_index, after_log_index, pool_a, pool_b, spread_bps, profit_wei, direction, verdict)`.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_intra_block_arbs(&self, rows: &[IntraBlockArbRow]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut inserted = 0usize;

        {
            let mut stmt = tx.prepare(
                "
                INSERT OR REPLACE INTO intra_block_arbs (
                    block_number,
                    after_tx_index,
                    after_log_index,
                    pool_a,
                    pool_b,
                    spread_bps,
                    profit_wei,
                    direction,
                    verdict
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;

            for row in rows {
                let affected = stmt.execute(rusqlite::params![
                    row.0 as i64,
                    row.1 as i64,
                    row.2 as i64,
                    row.3,
                    row.4,
                    row.5,
                    row.6,
                    row.7,
                    row.8,
                ])?;
                inserted = inserted.saturating_add(affected);
            }
        }

        tx.commit()?;
        Ok(inserted)
    }

    /// Batch insert mempool transactions using a prepared statement and transaction.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_mempool_txs(&self, txs: &[MempoolTransaction]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        let mut inserted_count = 0usize;
        {
            let mut stmt = tx.prepare(
                "
                INSERT OR IGNORE INTO mempool_transactions (
                    hash, block_number, timestamp_ms, from_address, to_address, value,
                    gas_limit, gas_price, max_fee_per_gas, max_priority_fee_per_gas,
                    nonce, input_data, tx_type, raw_tx
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;

            for t in txs {
                let affected = stmt.execute(rusqlite::params![
                    t.hash,
                    t.block_number,
                    t.timestamp_ms,
                    t.from_address,
                    t.to_address,
                    t.value,
                    t.gas_limit,
                    t.gas_price,
                    t.max_fee_per_gas,
                    t.max_priority_fee_per_gas,
                    t.nonce,
                    t.input_data,
                    t.tx_type,
                    t.raw_tx,
                ])?;
                inserted_count = inserted_count.saturating_add(affected);
            }
        }

        tx.commit()?;
        Ok(inserted_count)
    }

    /// Insert a single block.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_block(&self, block: &Block) -> Result<()> {
        self.conn.borrow_mut().execute(
            "
            INSERT INTO blocks (
                block_number, block_hash, parent_hash, timestamp, gas_limit,
                gas_used, base_fee_per_gas, miner, transaction_count
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
            rusqlite::params![
                block.block_number,
                block.block_hash,
                block.parent_hash,
                block.timestamp,
                block.gas_limit,
                block.gas_used,
                block.base_fee_per_gas,
                block.miner,
                block.transaction_count,
            ],
        )?;
        Ok(())
    }

    /// Batch insert block transactions using a prepared statement and transaction.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_block_txs(&self, txs: &[BlockTransaction]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "
                INSERT INTO block_transactions (
                    block_number, tx_hash, tx_index, from_address, to_address,
                    gas_used, effective_gas_price, status
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;

            for t in txs {
                stmt.execute(rusqlite::params![
                    t.block_number,
                    t.tx_hash,
                    t.tx_index,
                    t.from_address,
                    t.to_address,
                    t.gas_used,
                    t.effective_gas_price,
                    t.status,
                ])?;
            }
        }

        let count = txs.len();
        tx.commit()?;
        Ok(count)
    }

    /// Retrieve all mempool transactions for a given block number.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_mempool_txs_for_block(&self, block_number: u64) -> Result<Vec<MempoolTransaction>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT hash, block_number, timestamp_ms, from_address, to_address, value,
                   gas_limit, gas_price, max_fee_per_gas, max_priority_fee_per_gas,
                   nonce, input_data, tx_type, raw_tx
            FROM mempool_transactions
            WHERE block_number = ?
            ORDER BY nonce ASC
            ",
        )?;

        let txs = stmt
            .query_map(rusqlite::params![block_number], |row| {
                Ok(MempoolTransaction {
                    hash: row.get(0)?,
                    block_number: row.get(1)?,
                    timestamp_ms: row.get(2)?,
                    from_address: row.get(3)?,
                    to_address: row.get(4)?,
                    value: row.get(5)?,
                    gas_limit: row.get(6)?,
                    gas_price: row.get(7)?,
                    max_fee_per_gas: row.get(8)?,
                    max_priority_fee_per_gas: row.get(9)?,
                    nonce: row.get(10)?,
                    input_data: row.get(11)?,
                    tx_type: row.get(12)?,
                    raw_tx: row.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(txs)
    }

    /// Retrieve a block by block number.
    ///
    /// Returns `Ok(None)` if the block does not exist.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_block(&self, block_number: u64) -> Result<Option<Block>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT block_number, block_hash, parent_hash, timestamp, gas_limit,
                   gas_used, base_fee_per_gas, miner, transaction_count
            FROM blocks
            WHERE block_number = ?
            ",
        )?;

        let result = stmt.query_row(rusqlite::params![block_number], |row| {
            Ok(Block {
                block_number: row.get(0)?,
                block_hash: row.get(1)?,
                parent_hash: row.get(2)?,
                timestamp: row.get(3)?,
                gas_limit: row.get(4)?,
                gas_used: row.get(5)?,
                base_fee_per_gas: row.get(6)?,
                miner: row.get(7)?,
                transaction_count: row.get(8)?,
            })
        });

        match result {
            Ok(block) => Ok(Some(block)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e)?,
        }
    }

    /// Check if all blocks in range `start..=end` exist in the database.
    ///
    /// Returns `true` only if every block number in the inclusive range is present.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn block_range_exists(&self, start: u64, end: u64) -> Result<bool> {
        if start > end {
            return Ok(false);
        }

        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT COUNT(*) FROM blocks
            WHERE block_number >= ? AND block_number <= ?
            ",
        )?;

        let count: u64 = stmt.query_row(rusqlite::params![start, end], |row| row.get(0))?;
        let expected = end - start + 1;

        Ok(count == expected)
    }

    /// Retrieve all on-chain block transactions for a given block number.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_block_txs(&self, block_number: u64) -> Result<Vec<BlockTransaction>> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT block_number, tx_hash, tx_index, from_address, to_address,
                   gas_used, effective_gas_price, status
            FROM block_transactions
            WHERE block_number = ?
            ORDER BY tx_index ASC
            ",
        )?;

        let txs = stmt
            .query_map(rusqlite::params![block_number], |row| {
                Ok(BlockTransaction {
                    block_number: row.get(0)?,
                    tx_hash: row.get(1)?,
                    tx_index: row.get(2)?,
                    from_address: row.get(3)?,
                    to_address: row.get(4)?,
                    gas_used: row.get(5)?,
                    effective_gas_price: row.get(6)?,
                    status: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(txs)
    }

    /// Retrieve simulated block values by ordering algorithm for a block.
    ///
    /// Returns `(egp_simulated_total_value_wei, profit_simulated_total_value_wei, arbitrage_simulated_total_value_wei)`.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_simulated_values_for_block(
        &self,
        block_number: u64,
    ) -> Result<(Option<u128>, Option<u128>, Option<u128>)> {
        fn parse_wei(value: &str) -> u128 {
            if value.starts_with("0x") {
                u128::from_str_radix(value.trim_start_matches("0x"), 16).unwrap_or(0)
            } else {
                value.parse::<u128>().unwrap_or(0)
            }
        }

        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "
            SELECT ordering_algorithm, total_value_wei
            FROM simulation_results
            WHERE block_number = ?
            ",
        )?;

        let mut rows = stmt.query(rusqlite::params![block_number])?;
        let mut egp_value: Option<u128> = None;
        let mut profit_value: Option<u128> = None;
        let mut arbitrage_value: Option<u128> = None;

        while let Some(row) = rows.next()? {
            let algorithm: String = row.get(0)?;
            let total_value_wei: String = row.get(1)?;
            let parsed = parse_wei(&total_value_wei);

            match algorithm.to_lowercase().as_str() {
                "egp" => {
                    egp_value = Some(egp_value.unwrap_or(0).max(parsed));
                }
                "profit" => {
                    profit_value = Some(profit_value.unwrap_or(0).max(parsed));
                }
                "arbitrage" => {
                    arbitrage_value = Some(arbitrage_value.unwrap_or(0).max(parsed));
                }
                _ => {}
            }
        }

        Ok((egp_value, profit_value, arbitrage_value))
    }

    /// Insert a simulation result row.
    ///
    /// # Arguments
    /// * `block_number` - Block number simulated
    /// * `ordering_algorithm` - Algorithm used (e.g. "egp" or "profit")
    /// * `tx_count` - Number of transactions in the ordered result
    /// * `gas_used` - Total gas used
    /// * `total_value_wei` - Total simulated value in Wei (as hex string "0x...")
    /// * `mev_captured_wei` - MEV captured estimate in Wei (as hex string "0x...")
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_simulation_result(
        &self,
        block_number: u64,
        ordering_algorithm: &str,
        tx_count: usize,
        gas_used: u64,
        total_value_wei: &str,
        mev_captured_wei: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.borrow_mut();

        conn.execute(
            "
            DELETE FROM simulation_results
            WHERE block_number = ? AND ordering_algorithm = ?
            ",
            rusqlite::params![block_number, ordering_algorithm],
        )?;

        conn.execute(
            "
            INSERT INTO simulation_results (
                block_number, ordering_algorithm, simulated_at, tx_count, gas_used,
                total_value_wei, mev_captured_wei
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ",
            rusqlite::params![
                block_number,
                ordering_algorithm,
                now,
                tx_count as i64,
                gas_used as i64,
                total_value_wei,
                mev_captured_wei,
            ],
        )?;
        Ok(())
    }

    /// Get min/max block numbers from blocks table.
    ///
    /// Returns `(min_block, max_block, count)` or `(0, 0, 0)` if empty.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_block_range(&self) -> Result<(u64, u64, usize)> {
        let conn = self.conn.borrow();
        let mut stmt =
            conn.prepare("SELECT MIN(block_number), MAX(block_number), COUNT(*) FROM blocks")?;

        let (min_block, max_block, count): (Option<i64>, Option<i64>, i64) =
            stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

        Ok((
            min_block.map(|v| v as u64).unwrap_or(0),
            max_block.map(|v| v as u64).unwrap_or(0),
            count as usize,
        ))
    }

    /// Get min/max timestamps (ms) from mempool_transactions table.
    ///
    /// Returns `(min_ts_ms, max_ts_ms, count)` or `(0, 0, 0)` if empty.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_mempool_timestamp_range(&self) -> Result<(u64, u64, usize)> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(
            "SELECT MIN(timestamp_ms), MAX(timestamp_ms), COUNT(*) FROM mempool_transactions",
        )?;

        let (min_ts, max_ts, count): (Option<i64>, Option<i64>, i64) =
            stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

        Ok((
            min_ts.map(|v| v as u64).unwrap_or(0),
            max_ts.map(|v| v as u64).unwrap_or(0),
            count as usize,
        ))
    }

    /// Get count of simulation results.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn count_simulations(&self) -> Result<usize> {
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM simulation_results")?;

        let count: i64 = stmt.query_row([], |row| row.get(0))?;

        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_tables() {
        let store = Store::new(":memory:").expect("in-memory store should always open");
        let conn = store.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("query should prepare");

        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .expect("query_map should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("all rows should parse");

        assert!(tables.contains(&"block_transactions".to_string()));
        assert!(tables.contains(&"blocks".to_string()));
        assert!(tables.contains(&"cex_prices".to_string()));
        assert!(tables.contains(&"intra_block_arbs".to_string()));
        assert!(tables.contains(&"mempool_transactions".to_string()));
        assert!(tables.contains(&"mev_opportunities".to_string()));
        assert!(tables.contains(&"simulation_results".to_string()));
        assert!(tables.contains(&"tx_logs".to_string()));
    }

    #[test]
    fn insert_mempool_txs_batch() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let txs = vec![
            MempoolTransaction {
                hash: "0xaabbcc".to_string(),
                block_number: Some(100),
                timestamp_ms: 1000,
                from_address: "0xfrom1".to_string(),
                to_address: Some("0xto1".to_string()),
                value: "1000000".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 1,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0xdeadbeef".to_string(),
            },
            MempoolTransaction {
                hash: "0xddeeff".to_string(),
                block_number: Some(100),
                timestamp_ms: 1001,
                from_address: "0xfrom2".to_string(),
                to_address: Some("0xto2".to_string()),
                value: "2000000".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 2,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0xcafebabe".to_string(),
            },
        ];

        let count = store
            .insert_mempool_txs(&txs)
            .expect("insert should succeed");
        assert_eq!(count, 2);
    }

    #[test]
    fn insert_block_single() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let block = Block {
            block_number: 100,
            block_hash: "0xblockhash".to_string(),
            parent_hash: "0xparenthash".to_string(),
            timestamp: 1000,
            gas_limit: 30000000,
            gas_used: 15000000,
            base_fee_per_gas: "50000000000".to_string(),
            miner: "0xminer".to_string(),
            transaction_count: 100,
        };

        store.insert_block(&block).expect("insert should succeed");

        let conn = store.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT block_number FROM blocks WHERE block_number = ?")
            .expect("query should prepare");

        let found: Vec<u64> = stmt
            .query_map(rusqlite::params![100], |row| row.get(0))
            .expect("query_map should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("all rows should parse");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0], 100);
    }

    #[test]
    fn insert_block_txs_batch() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let txs = vec![
            BlockTransaction {
                block_number: 100,
                tx_hash: "0xtx1".to_string(),
                tx_index: 0,
                from_address: "0xfrom1".to_string(),
                to_address: "0xto1".to_string(),
                gas_used: 21000,
                effective_gas_price: "20000000000".to_string(),
                status: 1,
            },
            BlockTransaction {
                block_number: 100,
                tx_hash: "0xtx2".to_string(),
                tx_index: 1,
                from_address: "0xfrom2".to_string(),
                to_address: "0xto2".to_string(),
                gas_used: 50000,
                effective_gas_price: "20000000000".to_string(),
                status: 1,
            },
            BlockTransaction {
                block_number: 100,
                tx_hash: "0xtx3".to_string(),
                tx_index: 2,
                from_address: "0xfrom3".to_string(),
                to_address: "0xto3".to_string(),
                gas_used: 0,
                effective_gas_price: "20000000000".to_string(),
                status: 0,
            },
        ];

        let count = store.insert_block_txs(&txs).expect("insert should succeed");
        assert_eq!(count, 3);
    }

    #[test]
    fn get_mempool_txs_for_block_query() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let txs = vec![
            MempoolTransaction {
                hash: "0xaabbcc".to_string(),
                block_number: Some(100),
                timestamp_ms: 1000,
                from_address: "0xfrom1".to_string(),
                to_address: Some("0xto1".to_string()),
                value: "1000000".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 1,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0xdeadbeef".to_string(),
            },
            MempoolTransaction {
                hash: "0xddeeff".to_string(),
                block_number: Some(100),
                timestamp_ms: 1001,
                from_address: "0xfrom2".to_string(),
                to_address: Some("0xto2".to_string()),
                value: "2000000".to_string(),
                gas_limit: 21000,
                gas_price: "20000000000".to_string(),
                max_fee_per_gas: "0".to_string(),
                max_priority_fee_per_gas: "0".to_string(),
                nonce: 2,
                input_data: "0x".to_string(),
                tx_type: 0,
                raw_tx: "0xcafebabe".to_string(),
            },
        ];

        store
            .insert_mempool_txs(&txs)
            .expect("insert should succeed");

        let retrieved = store
            .get_mempool_txs_for_block(100)
            .expect("query should succeed");

        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0], txs[0]);
        assert_eq!(retrieved[1], txs[1]);
    }

    #[test]
    fn get_block_query() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let block = Block {
            block_number: 100,
            block_hash: "0xblockhash".to_string(),
            parent_hash: "0xparenthash".to_string(),
            timestamp: 1000,
            gas_limit: 30000000,
            gas_used: 15000000,
            base_fee_per_gas: "50000000000".to_string(),
            miner: "0xminer".to_string(),
            transaction_count: 100,
        };

        store.insert_block(&block).expect("insert should succeed");

        let retrieved = store.get_block(100).expect("query should succeed");

        assert_eq!(retrieved, Some(block));

        let missing = store.get_block(999).expect("query should succeed");

        assert_eq!(missing, None);
    }

    #[test]
    fn block_range_exists_check() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        // Insert blocks 100, 101, 102
        for block_num in 100..=102 {
            let block = Block {
                block_number: block_num,
                block_hash: format!("0xhash{}", block_num),
                parent_hash: format!("0xparent{}", block_num - 1),
                timestamp: 1000 + block_num,
                gas_limit: 30000000,
                gas_used: 15000000,
                base_fee_per_gas: "50000000000".to_string(),
                miner: "0xminer".to_string(),
                transaction_count: 10,
            };
            store.insert_block(&block).expect("insert should succeed");
        }

        // Range exists (all blocks present)
        let exists = store
            .block_range_exists(100, 102)
            .expect("query should succeed");
        assert!(exists);

        // Partial range (only 100-102 present)
        let partial = store
            .block_range_exists(100, 103)
            .expect("query should succeed");
        assert!(!partial);

        // Gap in range (103 missing, but 104-105 would be missing too)
        let gap = store
            .block_range_exists(101, 101)
            .expect("query should succeed");
        assert!(gap);

        // Empty range check (start > end)
        let invalid = store
            .block_range_exists(102, 100)
            .expect("query should succeed");
        assert!(!invalid);
    }

    #[test]
    fn insert_and_retrieve_tx_logs() {
        let store = Store::new(":memory:").expect("in-memory store should always open");

        let logs = vec![
            TxLog {
                block_number: 100,
                tx_hash: "0xaabbcc".to_string(),
                tx_index: 0,
                log_index: 0,
                address: "0xtoken1".to_string(),
                topic0: "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
                    .to_string(),
                topic1: Some("0x000000000000000000000000from_addr".to_string()),
                topic2: Some("0x0000000000000000000000000to__addr".to_string()),
                topic3: None,
                data: "0x0000000000000000000000000000000000000000000000000000000000000064"
                    .to_string(),
            },
            TxLog {
                block_number: 100,
                tx_hash: "0xaabbcc".to_string(),
                tx_index: 0,
                log_index: 1,
                address: "0xtoken2".to_string(),
                topic0: "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
                    .to_string(),
                topic1: None,
                topic2: None,
                topic3: None,
                data: "0x".to_string(),
            },
        ];

        let count = store.insert_tx_logs(&logs).expect("insert should succeed");
        assert_eq!(count, 2);

        // Retrieve by tx hash
        let by_tx = store
            .get_logs_for_tx("0xaabbcc")
            .expect("query should succeed");
        assert_eq!(by_tx.len(), 2);
        assert_eq!(by_tx[0].address, "0xtoken1");
        assert_eq!(by_tx[1].address, "0xtoken2");

        // Retrieve by block number
        let by_block = store.get_logs_for_block(100).expect("query should succeed");
        assert_eq!(by_block.len(), 2);

        // Retrieve only Transfer logs
        let transfers = store
            .get_transfer_logs_for_block(100)
            .expect("query should succeed");
        assert_eq!(transfers.len(), 1);
        assert_eq!(
            transfers[0].topic0,
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        );

        // Empty block returns empty vec
        let empty = store.get_logs_for_block(999).expect("query should succeed");
        assert!(empty.is_empty());
    }
}
