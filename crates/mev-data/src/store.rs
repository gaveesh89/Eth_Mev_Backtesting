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

#[allow(dead_code)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Creates or opens a SQLite database with WAL mode enabled.
    ///
    /// # Errors
    /// Returns error if the database cannot be opened or migrations fail.
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.execute_batch(
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_tables() {
        let store = Store::new(":memory:").expect("in-memory store should always open");
        let mut stmt = store
            .conn
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
}
