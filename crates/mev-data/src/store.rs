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

use crate::types::{Block, BlockTransaction, MempoolTransaction};

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
            ",
        )?;
        Ok(())
    }

    /// Batch insert mempool transactions using a prepared statement and transaction.
    ///
    /// # Errors
    /// Returns error if database insert fails.
    pub fn insert_mempool_txs(&self, txs: &[MempoolTransaction]) -> Result<usize> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "
                INSERT INTO mempool_transactions (
                    hash, block_number, timestamp_ms, from_address, to_address, value,
                    gas_limit, gas_price, max_fee_per_gas, max_priority_fee_per_gas,
                    nonce, input_data, tx_type, raw_tx
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;

            for t in txs {
                stmt.execute(rusqlite::params![
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
            }
        }

        let count = txs.len();
        tx.commit()?;
        Ok(count)
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
    /// Returns `(egp_simulated_total_value_wei, profit_simulated_total_value_wei)`.
    ///
    /// # Errors
    /// Returns error if database query fails.
    pub fn get_simulated_values_for_block(
        &self,
        block_number: u64,
    ) -> Result<(Option<u128>, Option<u128>)> {
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
                _ => {}
            }
        }

        Ok((egp_value, profit_value))
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
        self.conn.borrow_mut().execute(
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
        assert!(tables.contains(&"mempool_transactions".to_string()));
        assert!(tables.contains(&"mev_opportunities".to_string()));
        assert!(tables.contains(&"simulation_results".to_string()));
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
}
