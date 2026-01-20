mod migrations; // Declare the migrations module

use anyhow::Result;
use rusqlite::{Connection, ToSql, Transaction};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn new(connection: Connection) -> Result<Self> {
        crate::db::migrations::run_migrations(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn with_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction) -> Result<T>,
    {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire database lock"))?;
        let transaction = conn.transaction()?;
        let result = f(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn is_slot_locked(&self, contract_address: &str, slot_index: &[u8]) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire database lock"))?;
        let sql = is_slot_locked_query();
        let result = conn.query_row(
            &sql,
            rusqlite::params![contract_address, slot_index],
            |_| Ok(true),
        );

        match result {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn is_slot_locked_with_transaction(
        &self,
        transaction: &Transaction,
        contract_address: &str,
        slot_index: &[u8],
    ) -> Result<bool> {
        let sql = is_slot_locked_query();
        let result = transaction.query_row(
            &sql,
            rusqlite::params![contract_address, slot_index],
            |_| Ok(true),
        );

        match result {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn insert_slot_lock(&self, transaction: &Transaction, slot: &SlotInsertData) -> Result<()> {
        transaction.execute(
            "INSERT INTO slot_locks (
                start_block, btc_block, contract_address, slot_index, 
                slot_index_int, btc_txid, revert_value, current_value
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                slot.start_block,
                slot.btc_block,
                slot.contract_address,
                slot.slot_index,
                slot.slot_index_int,
                slot.btc_txid,
                slot.revert_value,
                slot.current_value,
            ],
        )?;

        Ok(())
    }

    pub fn get_slot_with_transaction(
        &self,
        transaction: &Transaction,
        contract_address: &str,
        slot_index: &[u8],
        current_block: u64,
    ) -> Result<Option<LockedSlot>> {
        let sql = get_slot_query();
        let result = transaction.query_row(
            &sql,
            rusqlite::params![contract_address, slot_index, current_block as i64],
            |row| {
                Ok(LockedSlot {
                    btc_txid: row.get(0)?,
                    btc_block: row.get(1)?,
                    contract_address: row.get(2)?,
                    slot_index: row.get(3)?,
                    revert_value: row.get(4)?,
                    current_value: row.get(5)?,
                    start_block: row.get(6)?,
                    end_block: row.get(7)?,
                })
            },
        );

        match result {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_slot(
        &self,
        contract_address: &str,
        slot_index: &[u8],
        current_block: u64,
    ) -> Result<Option<LockedSlot>> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire database lock"))?;
        let transaction = conn.transaction()?;
        self.get_slot_with_transaction(&transaction, contract_address, slot_index, current_block)
    }

    pub fn unlock_slot(
        &self,
        contract_address: &str,
        slot_index: &[u8],
        end_block: u64,
    ) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire database lock"))?;
        let transaction = conn.transaction()?;
        self.unlock_slot_with_transaction(&transaction, contract_address, slot_index, end_block)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn unlock_slot_with_transaction(
        &self,
        transaction: &Transaction,
        contract_address: &str,
        slot_index: &[u8],
        end_block: u64,
    ) -> Result<()> {
        let sql = unlock_slot_query();
        transaction.execute(
            &sql,
            rusqlite::params![end_block, contract_address, slot_index],
        )?;

        Ok(())
    }

    pub fn batch_insert_slot_locks(
        &self,
        transaction: &Transaction,
        slots: &[SlotInsertData],
    ) -> Result<Vec<bool>> {
        // Returns vec of success (false means already locked)
        let mut results = Vec::with_capacity(slots.len());

        // Check which slots are already locked
        for slot in slots {
            let is_locked = self.is_slot_locked_with_transaction(
                transaction,
                &slot.contract_address,
                slot.slot_index.as_slice(),
            )?;
            results.push(!is_locked);
        }

        // Filter out already locked slots
        let slots_to_insert: Vec<_> = slots
            .iter()
            .zip(results.iter())
            .filter(|(_, &can_insert)| can_insert)
            .map(|(slot, _)| slot)
            .collect();

        if !slots_to_insert.is_empty() {
            // Build multi-value insert query
            let values_str = "(?, ?, ?, ?, ?, ?, ?, ?)"
                .repeat(slots_to_insert.len())
                .split(")(")
                .collect::<Vec<_>>()
                .join("),(");

            let sql = format!(
                "INSERT INTO slot_locks (
                    start_block, btc_block, contract_address, slot_index, 
                    slot_index_int, btc_txid, revert_value, current_value
                ) VALUES {}",
                values_str,
            );

            // Flatten parameters
            let mut params: Vec<rusqlite::types::ToSqlOutput> =
                Vec::with_capacity(slots_to_insert.len() * 8);
            for slot in slots_to_insert {
                params.push((slot.start_block as i64).into());
                params.push((slot.btc_block as i64).into());
                params.push(slot.contract_address.as_str().into());
                params.push(slot.slot_index.as_slice().into());
                params.push(slot.slot_index_int.to_sql().unwrap());
                params.push(slot.btc_txid.as_str().into());
                params.push(slot.revert_value.as_slice().into());
                params.push(slot.current_value.as_slice().into());
            }

            transaction.execute(&sql, rusqlite::params_from_iter(params))?;
        }

        Ok(results)
    }

    pub fn batch_get_locked_slots(
        &self,
        transaction: &Transaction,
        slots: &[(&str, &[u8])], // Vec of (contract_address, slot_index)
        current_block: u64,      // Added parameter
    ) -> Result<Vec<Option<LockedSlot>>> {
        if slots.is_empty() {
            return Ok(Vec::new());
        }

        // Build query with multiple (contract_address, slot_index) pairs
        let placeholders = (1..=slots.len())
            .map(|i| {
                format!(
                    "(contract_address = ?{} AND slot_index = ?{})",
                    i * 2 - 1,
                    i * 2
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");

        let sql = format!(
            "SELECT btc_txid, btc_block, contract_address, slot_index, revert_value, current_value, start_block, end_block 
             FROM slot_locks 
             WHERE ({}) 
             AND (end_block IS NULL OR end_block = ?{})
             AND start_block <= ?{}",  // Added start_block constraint
            placeholders,
            slots.len() * 2 + 1,    // Parameter index for current_block in end_block check
            slots.len() * 2 + 1,    // Reuse parameter index for start_block check
        );

        // Flatten parameters
        let mut params: Vec<rusqlite::types::ToSqlOutput> = Vec::with_capacity(slots.len() * 2 + 2);
        for (addr, idx) in slots {
            params.push((*addr).into());
            params.push((*idx).into());
        }
        params.push((current_block as i64).into()); // Add current_block parameter for end_block check

        // Execute query and build result map
        let mut stmt = transaction.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(LockedSlot {
                btc_txid: row.get(0)?,
                btc_block: row.get(1)?,
                contract_address: row.get(2)?,
                slot_index: row.get(3)?,
                revert_value: row.get(4)?,
                current_value: row.get(5)?,
                start_block: row.get(6)?,
                end_block: row.get(7)?,
            })
        })?;

        // Build result map using both contract_address and slot_index as key
        let mut slot_map = std::collections::HashMap::new();
        for row in rows {
            let slot = row?;
            slot_map.insert(
                (slot.contract_address.clone(), slot.slot_index.clone()),
                slot,
            );
        }

        // Maintain input order
        Ok(slots
            .iter()
            .map(|(addr, idx)| {
                slot_map
                    .get(&((*addr).to_string(), (*idx).to_vec()))
                    .cloned()
            })
            .collect())
    }

    pub fn batch_unlock_slots(
        &self,
        transaction: &Transaction,
        slots: &[(&str, &[u8], u64)], // Vec of (contract_address, slot_index, end_block)
    ) -> Result<()> {
        if slots.is_empty() {
            return Ok(());
        }

        // Build multi-value update query with parameter indices:
        // ?1 is end_block (first parameter)
        // Then for each slot: ?2,?3 for first slot's addr/idx, ?4,?5 for second slot's addr/idx, etc
        let placeholders = (1..=slots.len())
            .map(|i| {
                format!(
                    "(contract_address = ?{} AND slot_index = ?{})",
                    i * 2,
                    i * 2 + 1
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");

        let sql = format!(
            "UPDATE slot_locks 
             SET end_block = ?1 
             WHERE ({}) AND end_block IS NULL",
            placeholders
        );

        // Flatten parameters
        let mut params: Vec<rusqlite::types::ToSqlOutput> = Vec::with_capacity(1 + slots.len() * 2);
        params.push((slots[0].2 as i64).into()); // end_block (same for all slots)
        for (addr, idx, _) in slots {
            params.push((*addr).into());
            params.push((*idx).into());
        }

        transaction.execute(&sql, rusqlite::params_from_iter(params))?;
        Ok(())
    }
}

// Helper function to get the SQL query for slot locks
fn is_slot_locked_query() -> String {
    "SELECT 1 FROM slot_locks 
     WHERE contract_address = ?1 
     AND slot_index = ?2 
     AND end_block IS NULL"
        .to_string()
}

// Helper function to get the SQL query for retrieving slot information
fn get_slot_query() -> String {
    "SELECT btc_txid, btc_block, contract_address, slot_index, revert_value, current_value, start_block, end_block 
     FROM slot_locks 
     WHERE contract_address = ?1 
     AND slot_index = ?2 
     AND (end_block IS NULL OR end_block = ?3)
     AND start_block <= ?3
     ORDER BY start_block, created_at DESC
     LIMIT 1"
        .to_string()
}

// Helper function to get the SQL query for unlocking a slot
fn unlock_slot_query() -> String {
    "UPDATE slot_locks 
     SET end_block = ?1 
     WHERE contract_address = ?2 
     AND slot_index = ?3 
     AND end_block IS NULL"
        .to_string()
}

#[derive(Debug, Clone)]
pub struct LockedSlot {
    pub btc_txid: String,
    pub btc_block: u64,
    pub contract_address: String,
    pub slot_index: Vec<u8>,
    pub revert_value: Vec<u8>,
    pub current_value: Vec<u8>,
    pub start_block: u64,
    pub end_block: Option<u64>,
}

#[derive(Debug)]
pub struct SlotInsertData {
    pub contract_address: String,
    pub start_block: u64,
    pub btc_block: u64,
    pub slot_index: Vec<u8>,
    pub slot_index_int: Option<i64>,
    pub btc_txid: String,
    pub revert_value: Vec<u8>,
    pub current_value: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Result<Database> {
        // Create in-memory database for testing
        let conn = Connection::open_in_memory()?;
        Database::new(conn)
    }

    #[test]
    fn test_slot_lock_operations() -> Result<()> {
        let db = setup_test_db()?;
        let contract_addr = "0x123";
        let slot_index = vec![1, 2, 3];
        let btc_txid = "txid123";
        let revert_value = vec![4, 5, 6];
        let current_value = vec![7, 8, 9];
        let start_block = 100;
        let btc_block = 200;

        // Test initial state
        assert!(!db.is_slot_locked(contract_addr, &slot_index)?);
        assert!(db
            .get_slot(contract_addr, &slot_index, start_block)?
            .is_none());

        // Test inserting a slot lock
        db.with_transaction(|tx| {
            let slot = SlotInsertData {
                contract_address: contract_addr.to_string(),
                start_block,
                btc_block,
                slot_index: slot_index.clone(),
                slot_index_int: None,
                btc_txid: btc_txid.to_string(),
                revert_value: revert_value.clone(),
                current_value: current_value.clone(),
            };
            db.insert_slot_lock(tx, &slot)
        })?;

        // Verify lock status
        assert!(db.is_slot_locked(contract_addr, &slot_index)?);

        // Test getting slot information
        let slot = db
            .get_slot(contract_addr, &slot_index, start_block)?
            .unwrap();
        assert_eq!(slot.btc_txid, btc_txid);
        assert_eq!(slot.btc_block, btc_block);
        assert_eq!(slot.contract_address, contract_addr);
        assert_eq!(slot.slot_index, slot_index);
        assert_eq!(slot.revert_value, revert_value);
        assert_eq!(slot.current_value, current_value);
        assert_eq!(slot.start_block, start_block);
        assert_eq!(slot.end_block, None);

        // Test unlocking the slot
        let end_block = 150;
        db.unlock_slot(contract_addr, &slot_index, end_block)?;

        // Verify unlock status
        assert!(!db.is_slot_locked(contract_addr, &slot_index)?);

        Ok(())
    }

    #[test]
    fn test_batch_operations() -> Result<()> {
        let db = setup_test_db()?;
        let slot_data: Vec<SlotInsertData> = vec![
            SlotInsertData {
                contract_address: "0x123".to_string(),
                start_block: 100,
                btc_block: 200,
                slot_index: vec![1, 2, 3],
                slot_index_int: None,
                btc_txid: "txid1".to_string(),
                revert_value: vec![4, 5, 6],
                current_value: vec![7, 8, 9],
            },
            SlotInsertData {
                contract_address: "0x456".to_string(),
                start_block: 101,
                btc_block: 201,
                slot_index: vec![2, 3, 4],
                slot_index_int: None,
                btc_txid: "txid2".to_string(),
                revert_value: vec![5, 6, 7],
                current_value: vec![8, 9, 10],
            },
        ];

        // Test batch insert
        db.with_transaction(|tx| {
            let results = db.batch_insert_slot_locks(tx, &slot_data)?;
            assert_eq!(results, vec![true, true]);
            Ok(())
        })?;

        // Test batch get with current_block = 99 (before start blocks)
        let get_indices = [vec![1, 2, 3], vec![2, 3, 4]];
        let get_slots = vec![
            ("0x123", get_indices[0].as_slice()),
            ("0x456", get_indices[1].as_slice()),
        ];

        db.with_transaction(|tx| {
            let results = db.batch_get_locked_slots(tx, &get_slots, 99)?;
            assert_eq!(results.len(), 2);
            assert!(results[0].is_none()); // Should be None because current_block < start_block
            assert!(results[1].is_none());
            Ok(())
        })?;

        // Test batch get with current_block = 101 (after both start blocks)
        db.with_transaction(|tx| {
            let results = db.batch_get_locked_slots(tx, &get_slots, 101)?;
            assert_eq!(results.len(), 2);
            assert!(results[0].is_some());
            assert!(results[1].is_some());

            let first_slot = results[0].as_ref().unwrap();
            assert_eq!(first_slot.btc_txid, "txid1");
            assert_eq!(first_slot.contract_address, "0x123");

            Ok(())
        })?;

        // Test batch get with current_block = 100 (equal to first start_block)
        db.with_transaction(|tx| {
            let results = db.batch_get_locked_slots(tx, &get_slots, 100)?;
            assert_eq!(results.len(), 2);
            assert!(results[0].is_some()); // First slot should be visible
            assert!(results[1].is_none()); // Second slot shouldn't be visible yet
            Ok(())
        })?;

        // Test batch unlock
        let unlock_slots = vec![
            ("0x123", get_indices[0].as_slice(), 150u64),
            ("0x456", get_indices[1].as_slice(), 150u64),
        ];

        db.with_transaction(|tx| {
            db.batch_unlock_slots(tx, &unlock_slots)?;
            Ok(())
        })?;

        // Verify unlocks
        assert!(!db.is_slot_locked("0x123", &[1, 2, 3])?);
        assert!(!db.is_slot_locked("0x456", &[2, 3, 4])?);

        Ok(())
    }

    #[test]
    fn test_concurrent_operations() -> Result<()> {
        let db = setup_test_db()?;
        let db_clone = db.clone();

        // Spawn a thread that tries to lock a slot
        let handle = std::thread::spawn(move || {
            db_clone.with_transaction(|tx| {
                let slot = SlotInsertData {
                    contract_address: "0x123".to_string(),
                    start_block: 100,
                    btc_block: 200,
                    slot_index: vec![1, 2, 3],
                    slot_index_int: None,
                    btc_txid: "txid1".to_string(),
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                };
                db_clone.insert_slot_lock(tx, &slot)
            })
        });

        // Try to lock the same slot in the main thread
        let _result = db.with_transaction(|tx| {
            let slot = SlotInsertData {
                contract_address: "0x123".to_string(),
                start_block: 101,
                btc_block: 201,
                slot_index: vec![1, 2, 3],
                slot_index_int: None,
                btc_txid: "txid2".to_string(),
                revert_value: vec![5, 6, 7],
                current_value: vec![8, 9, 10],
            };
            db.insert_slot_lock(tx, &slot)
        });

        // Wait for the spawned thread to complete
        handle.join().unwrap()?;

        // One of the operations should have failed due to the unique constraint
        assert!(db.is_slot_locked("0x123", &[1, 2, 3])?);

        Ok(())
    }

    #[test]
    fn test_get_slot_before_start_block() -> Result<()> {
        let db = setup_test_db()?;
        let contract_addr = "0x123";
        let slot_index = vec![1, 2, 3];
        let btc_txid = "txid123";
        let revert_value = vec![4, 5, 6];
        let current_value = vec![7, 8, 9];
        let start_block = 100;
        let btc_block = 200;

        // Insert a slot lock
        db.with_transaction(|tx| {
            let slot = SlotInsertData {
                contract_address: contract_addr.to_string(),
                start_block,
                btc_block,
                slot_index: slot_index.clone(),
                slot_index_int: None,
                btc_txid: btc_txid.to_string(),
                revert_value: revert_value.clone(),
                current_value: current_value.clone(),
            };
            db.insert_slot_lock(tx, &slot)
        })?;

        // Try to get slot at block 99 (before start_block)
        let slot = db.get_slot(contract_addr, &slot_index, 99)?;
        assert!(
            slot.is_none(),
            "Slot should not be visible before start_block"
        );

        // Get slot at start_block
        let slot = db.get_slot(contract_addr, &slot_index, start_block)?;
        assert!(slot.is_some(), "Slot should be visible at start_block");
        let slot = slot.unwrap();
        assert_eq!(slot.start_block, start_block);

        // Get slot after start_block
        let slot = db.get_slot(contract_addr, &slot_index, start_block + 1)?;
        assert!(slot.is_some(), "Slot should be visible after start_block");
        let slot = slot.unwrap();
        assert_eq!(slot.start_block, start_block);

        Ok(())
    }

    #[test]
    fn test_batch_get_locked_slots_before_start_block() -> Result<()> {
        let db = setup_test_db()?;
        let contract_addr = "0x123";
        let slot_index_1 = vec![1, 2, 3];
        let slot_index_2 = vec![4, 5, 6];
        let btc_txid = "txid123";
        let revert_value = vec![4, 5, 6];
        let current_value = vec![7, 8, 9];
        let start_block = 100;
        let btc_block = 200;

        // Insert two slot locks with the same start block
        db.with_transaction(|tx| {
            let slot1 = SlotInsertData {
                contract_address: contract_addr.to_string(),
                start_block,
                btc_block,
                slot_index: slot_index_1.clone(),
                slot_index_int: None,
                btc_txid: btc_txid.to_string(),
                revert_value: revert_value.clone(),
                current_value: current_value.clone(),
            };
            db.insert_slot_lock(tx, &slot1)?;
            let slot2 = SlotInsertData {
                contract_address: contract_addr.to_string(),
                start_block,
                btc_block,
                slot_index: slot_index_2.clone(),
                slot_index_int: None,
                btc_txid: btc_txid.to_string(),
                revert_value: revert_value.clone(),
                current_value: current_value.clone(),
            };
            db.insert_slot_lock(tx, &slot2)
        })?;

        let slots = vec![
            (contract_addr, slot_index_1.as_slice()),
            (contract_addr, slot_index_2.as_slice()),
        ];

        // Try to get slots at block 99 (before start_block)
        let result = db.with_transaction(|tx| db.batch_get_locked_slots(tx, &slots, 99))?;
        assert_eq!(result.len(), 2);
        assert!(
            result[0].is_none(),
            "First slot should not be visible before start_block"
        );
        assert!(
            result[1].is_none(),
            "Second slot should not be visible before start_block"
        );

        // Get slots at start_block
        let result =
            db.with_transaction(|tx| db.batch_get_locked_slots(tx, &slots, start_block))?;
        assert_eq!(result.len(), 2);
        assert!(
            result[0].is_some(),
            "First slot should be visible at start_block"
        );
        assert!(
            result[1].is_some(),
            "Second slot should be visible at start_block"
        );
        assert_eq!(result[0].as_ref().unwrap().start_block, start_block);
        assert_eq!(result[1].as_ref().unwrap().start_block, start_block);

        // Get slots after start_block
        let result =
            db.with_transaction(|tx| db.batch_get_locked_slots(tx, &slots, start_block + 1))?;
        assert_eq!(result.len(), 2);
        assert!(
            result[0].is_some(),
            "First slot should be visible after start_block"
        );
        assert!(
            result[1].is_some(),
            "Second slot should be visible after start_block"
        );
        assert_eq!(result[0].as_ref().unwrap().start_block, start_block);
        assert_eq!(result[1].as_ref().unwrap().start_block, start_block);

        Ok(())
    }
}
