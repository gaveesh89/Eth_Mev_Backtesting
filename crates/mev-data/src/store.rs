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
}
