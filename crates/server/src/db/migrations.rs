use anyhow::Result;
use rusqlite::Connection;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    // Create tables if they don't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS slot_locks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            start_block INTEGER NOT NULL,
            end_block INTEGER,
            btc_block INTEGER NOT NULL,
            contract_address TEXT NOT NULL,
            slot_index BLOB NOT NULL,
            slot_index_int INTEGER,
            btc_txid TEXT NOT NULL,
            revert_value BLOB NOT NULL,
            current_value BLOB NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            -- Removed for development
            -- UNIQUE(contract_address, slot_index, end_block)
        )",
        [],
    )?;

    // Create triggers for automatic timestamp updates
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS update_slot_locks_timestamp 
         AFTER UPDATE ON slot_locks
         FOR EACH ROW
         BEGIN
             UPDATE slot_locks SET updated_at = CURRENT_TIMESTAMP
             WHERE rowid = NEW.rowid;
         END;",
        [],
    )?;

    Ok(())
}
