use crate::db::{Database, SlotInsertData};
use crate::service::bitcoin::BitcoinRpcServiceAPI;
use hex;
use sova_sentinel_proto::proto::{
    get_slot_status_response, lock_slot_response,
    slot_lock_service_server::{SlotLockService, SlotLockServiceServer},
    slot_lock_status, BatchGetSlotStatusRequest, BatchGetSlotStatusResponse, BatchLockSlotRequest,
    BatchLockSlotResponse, BatchUnlockSlotRequest, BatchUnlockSlotResponse, GetSlotStatusRequest,
    GetSlotStatusResponse, LockSlotRequest, LockSlotResponse, SlotLockStatus,
};
use tonic::{Request, Response, Status};

pub struct SlotLockServiceImpl<B: BitcoinRpcServiceAPI> {
    db: Database,
    bitcoin_service: B,
    revert_threshold: u32,
}

impl<B: BitcoinRpcServiceAPI> SlotLockServiceImpl<B> {
    pub fn new(db: Database, bitcoin_service: B, revert_threshold: u32) -> Self {
        Self {
            db,
            bitcoin_service,
            revert_threshold,
        }
    }

    pub fn into_service(self) -> SlotLockServiceServer<Self> {
        SlotLockServiceServer::new(self)
    }
}

// Add this helper function near the top of the file, after the imports
fn format_bytes(bytes: &[u8]) -> String {
    if bytes.len() <= 8 {
        // Try to parse as u64/i64 first
        if bytes.is_empty() {
            return "[]".to_string();
        }
        let mut buf = [0u8; 8];
        buf[8 - bytes.len()..].copy_from_slice(bytes);
        let num = u64::from_be_bytes(buf);
        format!("{}(0x{:x})", num, num)
    } else {
        // Otherwise format as hex
        format!("0x{}", hex::encode(bytes))
    }
}

// Add this helper struct for better debug formatting
#[derive(Debug)]
#[allow(dead_code)]
struct FormattedSlot<'a> {
    contract_address: &'a str,
    slot_index: String,
    btc_txid: Option<&'a str>,
}

impl<'a> FormattedSlot<'a> {
    fn from_request_slot(slot: &'a sova_sentinel_proto::proto::SlotData) -> Self {
        Self {
            contract_address: &slot.contract_address,
            slot_index: format_bytes(&slot.slot_index),
            btc_txid: Some(&slot.btc_txid),
        }
    }

    fn from_identifier(slot: &'a sova_sentinel_proto::proto::SlotIdentifier) -> Self {
        Self {
            contract_address: &slot.contract_address,
            slot_index: format_bytes(&slot.slot_index),
            btc_txid: None,
        }
    }
}

// Add these helper functions after the imports
fn lock_status_to_string(status: i32) -> &'static str {
    match status {
        x if x == slot_lock_status::Status::Locked as i32 => "Locked",
        x if x == slot_lock_status::Status::AlreadyLocked as i32 => "AlreadyLocked",
        _ => "Unknown",
    }
}

fn get_status_to_string(status: i32) -> &'static str {
    match status {
        x if x == get_slot_status_response::Status::Unlocked as i32 => "Unlocked",
        x if x == get_slot_status_response::Status::Locked as i32 => "Locked",
        x if x == get_slot_status_response::Status::Reverted as i32 => "Reverted",
        _ => "Unknown",
    }
}

#[tonic::async_trait]
impl<B: BitcoinRpcServiceAPI + 'static> SlotLockService for SlotLockServiceImpl<B> {
    async fn lock_slot(
        &self,
        request: Request<LockSlotRequest>,
    ) -> Result<Response<LockSlotResponse>, Status> {
        let req = request.into_inner();

        tracing::info!(
            "LockSlot request: contract={}, slot={}, locked_at_block={}, btc_block={}, btc_txid={}",
            req.contract_address,
            format_bytes(&req.slot_index),
            req.locked_at_block,
            req.btc_block,
            req.btc_txid
        );

        let result = self
            .db
            .with_transaction(|transaction| {
                // Check if slot is already locked within the transaction
                let is_locked = self
                    .db
                    .is_slot_locked_with_transaction(
                        transaction,
                        &req.contract_address,
                        &req.slot_index,
                    )
                    .map_err(|e| anyhow::anyhow!("Database error: {}", e))?;

                if is_locked {
                    return Ok(lock_slot_response::Status::AlreadyLocked as i32);
                }

                // Try to parse slot_index as u64 for optional integer storage
                let slot_index_int = if req.slot_index.len() <= 8 {
                    let mut bytes = [0u8; 8];
                    bytes[8 - req.slot_index.len()..].copy_from_slice(&req.slot_index);
                    Some(i64::from_be_bytes(bytes))
                } else {
                    None
                };

                // Insert new lock
                let slot = SlotInsertData {
                    contract_address: req.contract_address.clone(),
                    start_block: req.locked_at_block,
                    btc_block: req.btc_block,
                    slot_index: req.slot_index.clone(),
                    slot_index_int,
                    btc_txid: req.btc_txid.clone(),
                    revert_value: req.revert_value.clone(),
                    current_value: req.current_value.clone(),
                };
                self.db.insert_slot_lock(transaction, &slot)?;

                Ok(lock_slot_response::Status::Locked as i32)
            })
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?;

        tracing::info!(
            "LockSlot response: contract={}, slot={}, status={}",
            req.contract_address,
            format_bytes(&req.slot_index),
            lock_status_to_string(result)
        );

        Ok(Response::new(LockSlotResponse {
            status: result,
            contract_address: req.contract_address,
            slot_index: req.slot_index,
        }))
    }

    async fn get_slot_status(
        &self,
        request: Request<GetSlotStatusRequest>,
    ) -> Result<Response<GetSlotStatusResponse>, Status> {
        let req = request.into_inner();

        tracing::info!(
            "GetSlotStatus request: contract={}, slot={}, current_block={}, btc_block={}",
            req.contract_address,
            format_bytes(&req.slot_index),
            req.current_block,
            req.btc_block
        );

        // Get slot info for Bitcoin RPC calls
        let slot = self
            .db
            .get_slot(&req.contract_address, &req.slot_index, req.current_block)
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?;

        // Early return if no slot found
        let Some(slot_info) = slot else {
            return Ok(Response::new(GetSlotStatusResponse {
                status: get_slot_status_response::Status::Unlocked as i32,
                contract_address: req.contract_address,
                slot_index: req.slot_index,
                revert_value: Vec::new(),
                current_value: Vec::new(),
            }));
        };

        let block_delta = req.btc_block - slot_info.btc_block;

        // Check if slot was already unlocked in a previous call (end_block is set)
        // If so, we need to return a consistent status based on when it was unlocked:
        // - Reverted: if the unlock happened due to exceeding the revert threshold
        // - Unlocked: if the unlock happened due to successful BTC confirmation
        // This ensures the same request always gets the same response after unlock
        if slot_info.end_block.is_some() {
            let status = if block_delta > self.revert_threshold as u64 {
                get_slot_status_response::Status::Reverted as i32
            } else {
                get_slot_status_response::Status::Unlocked as i32
            };

            return Ok(Response::new(GetSlotStatusResponse {
                status,
                contract_address: req.contract_address,
                slot_index: req.slot_index,
                revert_value: Vec::new(),
                current_value: Vec::new(),
            }));
        }

        // Check confirmation status if slot exists and is not unlocked
        let confirmation_status = self
            .bitcoin_service
            .is_tx_confirmed(&slot_info.btc_txid)
            .await
            .map_err(|e| Status::internal(format!("Bitcoin RPC error: {}", e)))?;

        tracing::debug!(
            "Bitcoin tx confirmation check: txid={}, confirmed={}",
            slot_info.btc_txid,
            confirmation_status
        );

        // Do everything else within a transaction
        let (status, revert_value, current_value) = self
            .db
            .with_transaction(|transaction| {
                let slot = self
                    .db
                    .get_slot_with_transaction(
                        transaction,
                        &req.contract_address,
                        &req.slot_index,
                        req.current_block,
                    )
                    .map_err(|e| anyhow::anyhow!("Database error: {}", e))?;

                match slot {
                    Some(slot) => {
                        if block_delta > self.revert_threshold as u64 {
                            tracing::debug!(
                                "Reverting slot: contract={}, slot={}, btc_blocks_passed={}",
                                req.contract_address,
                                format_bytes(&req.slot_index),
                                block_delta
                            );
                            self.db.unlock_slot_with_transaction(
                                transaction,
                                &req.contract_address,
                                &req.slot_index,
                                req.current_block,
                            )?;
                            Ok((
                                get_slot_status_response::Status::Reverted as i32,
                                slot.revert_value,
                                slot.current_value,
                            ))
                        } else if confirmation_status {
                            tracing::debug!(
                                "Unlocking slot: contract={}, slot={}, btc_tx_confirmed=true",
                                req.contract_address,
                                format_bytes(&req.slot_index)
                            );
                            self.db.unlock_slot_with_transaction(
                                transaction,
                                &req.contract_address,
                                &req.slot_index,
                                req.current_block,
                            )?;
                            Ok((
                                get_slot_status_response::Status::Unlocked as i32,
                                Vec::new(),
                                Vec::new(),
                            ))
                        } else {
                            tracing::debug!(
                                "Slot remains locked: contract={}, slot={}, btc_blocks_passed={}",
                                req.contract_address,
                                format_bytes(&req.slot_index),
                                block_delta,
                            );
                            Ok((
                                get_slot_status_response::Status::Locked as i32,
                                Vec::new(),
                                Vec::new(),
                            ))
                        }
                    }
                    None => {
                        tracing::debug!(
                            "Slot not found (unlocked): contract={}, slot={}",
                            req.contract_address,
                            format_bytes(&req.slot_index)
                        );
                        Ok((
                            get_slot_status_response::Status::Unlocked as i32,
                            Vec::new(),
                            Vec::new(),
                        ))
                    }
                }
            })
            .map_err(|e| Status::internal(format!("{}", e)))?;

        tracing::info!(
            "GetSlotStatus response: contract={}, slot={}, status={}",
            req.contract_address,
            format_bytes(&req.slot_index),
            get_status_to_string(status)
        );

        Ok(Response::new(GetSlotStatusResponse {
            status,
            contract_address: req.contract_address,
            slot_index: req.slot_index,
            revert_value,
            current_value,
        }))
    }

    async fn batch_lock_slot(
        &self,
        request: Request<BatchLockSlotRequest>,
    ) -> Result<Response<BatchLockSlotResponse>, Status> {
        let req = request.into_inner();

        // Return early if slots array is empty
        if req.slots.is_empty() {
            return Ok(Response::new(BatchLockSlotResponse { slots: vec![] }));
        }

        // Log the request payload with formatted slots
        let formatted_slots: Vec<_> = req
            .slots
            .iter()
            .map(FormattedSlot::from_request_slot)
            .collect();

        tracing::info!(
            "BatchLockSlot request: locked_at_block={}, btc_block={}, slots={:#?}",
            req.locked_at_block,
            req.btc_block,
            formatted_slots
        );

        let result = self
            .db
            .with_transaction(|transaction| {
                // Get all slot locks in one query
                let slots_to_check: Vec<_> = req
                    .slots
                    .iter()
                    .map(|slot| (slot.contract_address.as_str(), slot.slot_index.as_slice()))
                    .collect();

                let existing_slots = self.db.batch_get_locked_slots(
                    transaction,
                    &slots_to_check,
                    req.locked_at_block,
                )?;

                let mut responses = Vec::with_capacity(req.slots.len());
                let mut slots_to_insert = Vec::with_capacity(req.slots.len());

                // Process each slot using the batch query results
                for (idx, slot) in req.slots.iter().enumerate() {
                    if existing_slots[idx].is_some() {
                        responses.push(SlotLockStatus {
                            contract_address: slot.contract_address.clone(),
                            slot_index: slot.slot_index.clone(),
                            status: slot_lock_status::Status::AlreadyLocked as i32,
                        });
                        continue;
                    }

                    // Try to parse slot_index as u64 for optional integer storage
                    let slot_index_int = if slot.slot_index.len() <= 8 {
                        let mut bytes = [0u8; 8];
                        bytes[8 - slot.slot_index.len()..].copy_from_slice(&slot.slot_index);
                        Some(i64::from_be_bytes(bytes))
                    } else {
                        None
                    };

                    slots_to_insert.push(SlotInsertData {
                        contract_address: slot.contract_address.clone(),
                        start_block: req.locked_at_block,
                        btc_block: req.btc_block,
                        slot_index: slot.slot_index.clone(),
                        slot_index_int,
                        btc_txid: slot.btc_txid.clone(),
                        revert_value: slot.revert_value.clone(),
                        current_value: slot.current_value.clone(),
                    });

                    responses.push(SlotLockStatus {
                        contract_address: slot.contract_address.clone(),
                        slot_index: slot.slot_index.clone(),
                        status: slot_lock_status::Status::Locked as i32,
                    });
                }

                // Insert all slots that can be locked
                if !slots_to_insert.is_empty() {
                    self.db
                        .batch_insert_slot_locks(transaction, &slots_to_insert)?;
                }

                Ok(responses)
            })
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?;

        // Format the response slots
        let formatted_response: Vec<_> = result
            .iter()
            .map(|status| {
                format!(
                    "{{ contract: {}, slot: {}, status: {} }}",
                    status.contract_address,
                    format_bytes(&status.slot_index),
                    lock_status_to_string(status.status)
                )
            })
            .collect();

        tracing::info!("BatchLockSlot response: slots={:#?}", formatted_response);

        Ok(Response::new(BatchLockSlotResponse { slots: result }))
    }

    async fn batch_get_slot_status(
        &self,
        request: Request<BatchGetSlotStatusRequest>,
    ) -> Result<Response<BatchGetSlotStatusResponse>, Status> {
        let req = request.into_inner();

        // Return early if slots array is empty
        if req.slots.is_empty() {
            return Ok(Response::new(BatchGetSlotStatusResponse { slots: vec![] }));
        }

        // Log the request payload with formatted slots
        let formatted_slots: Vec<_> = req
            .slots
            .iter()
            .map(FormattedSlot::from_identifier)
            .collect();

        tracing::info!(
            "BatchGetSlotStatus request: current_block={}, btc_block={}, slots={:#?}",
            req.current_block,
            req.btc_block,
            formatted_slots
        );

        // Convert slots to database format
        let slots: Vec<_> = req
            .slots
            .iter()
            .map(|slot| (slot.contract_address.as_str(), slot.slot_index.as_slice()))
            .collect();

        let existing_slots = self
            .db
            .with_transaction(|transaction| {
                self.db
                    .batch_get_locked_slots(transaction, &slots, req.current_block)
            })
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?;

        // Filter slots into unlocked (slots unlocked at this sova block) and locked arrays
        let (unlocked_slots, active_slots): (Vec<_>, Vec<_>) = existing_slots
            .iter()
            .enumerate()
            // filter out None values, aka not locked slots
            .filter_map(|(idx, slot)| slot.as_ref().map(|s| (idx, s)))
            .partition(|(_, slot)| slot.end_block.is_some());

        // For unlocked slots, check if they were reverted
        let mut initial_slots: Vec<GetSlotStatusResponse> = unlocked_slots
            .iter()
            .map(|(_, slot)| {
                let block_delta = req.btc_block - slot.btc_block;

                GetSlotStatusResponse {
                    status: if block_delta > self.revert_threshold as u64 {
                        get_slot_status_response::Status::Reverted as i32
                    } else {
                        get_slot_status_response::Status::Unlocked as i32
                    },
                    contract_address: slot.contract_address.clone(),
                    slot_index: slot.slot_index.clone(),
                    revert_value: if block_delta > self.revert_threshold as u64 {
                        slot.revert_value.clone()
                    } else {
                        Vec::new()
                    },
                    current_value: if block_delta > self.revert_threshold as u64 {
                        slot.current_value.clone()
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect();

        // Add responses for slots that were never locked
        let mut not_locked_responses: Vec<GetSlotStatusResponse> = req
            .slots
            .iter()
            .enumerate()
            .filter(|(idx, _)| existing_slots[*idx].is_none())
            .map(|(_, slot_req)| GetSlotStatusResponse {
                status: get_slot_status_response::Status::Unlocked as i32,
                contract_address: slot_req.contract_address.clone(),
                slot_index: slot_req.slot_index.clone(),
                revert_value: Vec::new(),
                current_value: Vec::new(),
            })
            .collect();

        // Check if the number of active slots is 0, then we can early return
        if active_slots.is_empty() {
            initial_slots.append(&mut not_locked_responses);

            // Format the response slots before logging
            let format_response_slot = |slot: &GetSlotStatusResponse| {
                format!(
                    "{{ contract: {}, slot: {}, status: {} }}",
                    slot.contract_address,
                    format_bytes(&slot.slot_index),
                    get_status_to_string(slot.status)
                )
            };

            let formatted_response: Vec<_> =
                initial_slots.iter().map(format_response_slot).collect();

            tracing::info!(
                "BatchGetSlotStatus response: slots={:#?}",
                formatted_response
            );

            return Ok(Response::new(BatchGetSlotStatusResponse {
                slots: initial_slots,
            }));
        }

        // We have active slots, so we need to check confirmation status for each txid
        // Collect unique txids from active slots
        let unique_txids: std::collections::HashSet<_> = active_slots
            .iter()
            .map(|(_, slot)| slot.btc_txid.clone())
            .collect();

        // Check confirmation status for unique active txids in parallel
        let confirmation_futures: Vec<_> = unique_txids
            .iter()
            .map(|txid| async move {
                self.bitcoin_service
                    .is_tx_confirmed(txid)
                    .await
                    .map(|confirmed| (txid.clone(), confirmed))
                    .map_err(|e| Status::internal(format!("Bitcoin RPC error: {}", e)))
            })
            .collect();

        // Execute all confirmation futures in parallel and collect results into a HashMap
        let confirmation_statuses: std::collections::HashMap<_, _> =
            futures::future::try_join_all(confirmation_futures)
                .await?
                .into_iter()
                .collect();

        // Map confirmation results back to active slots
        let slot_confirmations: Vec<_> = active_slots
            .iter()
            .map(|(_, slot)| {
                confirmation_statuses
                    .get(&slot.btc_txid)
                    .copied()
                    .unwrap_or(false)
            })
            .collect();

        // Process results and update DB in same transaction
        let locked_slots = self
            .db
            .with_transaction(|transaction| {
                let mut slots = Vec::with_capacity(active_slots.len());
                let mut slots_to_unlock = Vec::new();

                // First pass: collect confirmation statuses and slots
                for ((_, slot), is_confirmed) in active_slots.iter().zip(slot_confirmations.iter())
                {
                    let block_delta = req.btc_block - slot.btc_block;

                    let (status, revert_value, current_value) =
                        if block_delta > self.revert_threshold as u64 || *is_confirmed {
                            // Slot needs to be unlocked for one of two reasons:
                            // 1. Bitcoin block delta exceeded revert threshold (too many blocks passed)
                            // 2. Bitcoin transaction is confirmed
                            slots_to_unlock.push((
                                slot.contract_address.as_str(),
                                slot.slot_index.as_slice(),
                                req.current_block,
                            ));

                            if block_delta > self.revert_threshold as u64 {
                                // Slot is being unlocked because too many BTC blocks passed without confirmation
                                // In this case, we report it as "Reverted" and include the revert values
                                (
                                    get_slot_status_response::Status::Reverted as i32,
                                    slot.revert_value.clone(),
                                    slot.current_value.clone(),
                                )
                            } else {
                                // Slot is being unlocked because the Bitcoin transaction was confirmed
                                // In this case, we report it as "Unlocked" and don't need values
                                (
                                    get_slot_status_response::Status::Unlocked as i32,
                                    Vec::new(),
                                    Vec::new(),
                                )
                            }
                        } else {
                            // Slot is locked and active:
                            // - Current block has reached or passed start block
                            // - Bitcoin transaction is not yet confirmed
                            // - Bitcoin block delta has not exceeded revert threshold
                            (
                                get_slot_status_response::Status::Locked as i32,
                                Vec::new(),
                                Vec::new(),
                            )
                        };

                    slots.push(GetSlotStatusResponse {
                        status,
                        contract_address: slot.contract_address.clone(),
                        slot_index: slot.slot_index.clone(),
                        revert_value,
                        current_value,
                    });
                }

                // Batch unlock all slots that need unlocking
                if !slots_to_unlock.is_empty() {
                    self.db.batch_unlock_slots(transaction, &slots_to_unlock)?;
                }

                Ok(slots)
            })
            .map_err(|e| Status::internal(format!("{}", e)))?;

        // Combine all responses
        let mut all_slots = initial_slots;
        all_slots.extend(locked_slots);
        all_slots.extend(not_locked_responses);

        // Format the response slots before logging
        let format_response_slot = |slot: &GetSlotStatusResponse| {
            format!(
                "{{ contract: {}, slot: {}, status: {} }}",
                slot.contract_address,
                format_bytes(&slot.slot_index),
                get_status_to_string(slot.status)
            )
        };

        let formatted_response: Vec<_> = all_slots.iter().map(format_response_slot).collect();

        tracing::info!(
            "BatchGetSlotStatus response: slots={:#?}",
            formatted_response
        );

        Ok(Response::new(BatchGetSlotStatusResponse {
            slots: all_slots,
        }))
    }

    async fn batch_unlock_slot(
        &self,
        request: Request<BatchUnlockSlotRequest>,
    ) -> Result<Response<BatchUnlockSlotResponse>, Status> {
        let req = request.into_inner();

        // Return early if slots array is empty
        if req.slots.is_empty() {
            return Ok(Response::new(BatchUnlockSlotResponse { slots: vec![] }));
        }

        tracing::info!(
            "BatchUnlockSlot request: current_block={}, btc_block={}, slot_count={}",
            req.current_block,
            req.btc_block,
            req.slots.len()
        );

        // Convert slots to database format
        let slots_to_unlock: Vec<_> = req
            .slots
            .iter()
            .map(|slot| {
                (
                    slot.contract_address.as_str(),
                    slot.slot_index.as_slice(),
                    req.current_block,
                )
            })
            .collect();

        // Unlock slots in a transaction
        self.db
            .with_transaction(|transaction| {
                self.db.batch_unlock_slots(transaction, &slots_to_unlock)
            })
            .map_err(|e| Status::internal(format!("Database error: {}", e)))?;

        // Transform slots back to response format
        let slots = req.slots.to_vec();

        tracing::info!("BatchUnlockSlot response: unlocked {} slots", slots.len());

        Ok(Response::new(BatchUnlockSlotResponse { slots }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sova_sentinel_proto::proto::{SlotData, SlotIdentifier};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockBitcoinService {
        confirmed_txs: Arc<Mutex<Vec<String>>>,
    }

    impl MockBitcoinService {
        fn new() -> Self {
            Self {
                confirmed_txs: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn add_confirmed_tx(&self, txid: &str) {
            let mut txs = self.confirmed_txs.lock().unwrap();
            println!("adding confirmed tx: {}", txid);
            txs.push(txid.to_string());
        }
    }

    #[tonic::async_trait]
    impl BitcoinRpcServiceAPI for MockBitcoinService {
        async fn is_tx_confirmed(&self, txid: &str) -> anyhow::Result<bool> {
            let txs = self.confirmed_txs.lock().unwrap();
            println!("txid: {}, confirmed_txs: {:?}", txid, *txs);
            Ok(txs.contains(&txid.to_string()))
        }
    }

    #[tokio::test]
    async fn test_lock_slot() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc, 6);

        let request = Request::new(LockSlotRequest {
            locked_at_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });

        // Test successful lock
        let response = service.lock_slot(request).await?;
        assert_eq!(
            response.get_ref().status,
            lock_slot_response::Status::Locked as i32
        );

        // Test already locked
        let request = Request::new(LockSlotRequest {
            locked_at_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid2".to_string(),
        });

        let response = service.lock_slot(request).await?;
        assert_eq!(
            response.get_ref().status,
            lock_slot_response::Status::AlreadyLocked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_slot_status_unlocked() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Lock a slot first
        let lock_request = Request::new(LockSlotRequest {
            locked_at_block: 1000,
            btc_block: 95,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });
        service.lock_slot(lock_request).await?;

        // Test locked status
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1001,
            btc_block: 96,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Locked as i32
        );

        // Can modify mock after it's moved
        btc.add_confirmed_tx("txid1");

        // Test confirmed transaction
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1002,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Unlocked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_slot_status_revert() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Lock a slot at btc_block 100
        let lock_request = Request::new(LockSlotRequest {
            locked_at_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });
        service.lock_slot(lock_request).await?;

        // Check status - should be reverted since block delta > 6
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1000,
            btc_block: 110,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(response.get_ref().revert_value, vec![4, 5, 6]);
        assert_eq!(response.get_ref().current_value, vec![7, 8, 9]);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_slot_status_locked() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Lock a slot
        let lock_request = Request::new(LockSlotRequest {
            locked_at_block: 1000,
            btc_block: 98, // Only 2 blocks old
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });
        service.lock_slot(lock_request).await?;

        // Check status - should be locked since block delta < 6 and tx not confirmed
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Locked as i32
        );
        assert!(response.get_ref().revert_value.is_empty());
        assert!(response.get_ref().current_value.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_operations() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Test batch lock
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 95,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                    revert_value: vec![5, 6, 7],
                    current_value: vec![8, 9, 10],
                    btc_txid: "txid2".to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_lock_slot() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Test initial batch lock
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 95,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                    revert_value: vec![5, 6, 7],
                    current_value: vec![8, 9, 10],
                    btc_txid: "txid2".to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        // Test attempting to lock already locked slots
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 95,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![1, 1, 1],
                    current_value: vec![2, 2, 2],
                    btc_txid: "txid3".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x789".to_string(), // New slot
                    slot_index: vec![3, 4, 5],
                    revert_value: vec![6, 7, 8],
                    current_value: vec![9, 10, 11],
                    btc_txid: "txid4".to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::AlreadyLocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_get_slot_status_unlocked() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // First lock some slots
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 95,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                    revert_value: vec![5, 6, 7],
                    current_value: vec![8, 9, 10],
                    btc_txid: "txid1".to_string(),
                },
            ],
        });
        service.batch_lock_slot(request).await?;

        // Confirm the transaction
        btc.add_confirmed_tx("txid1");

        // Check status - should be unlocked since tx is confirmed
        let request = Request::new(BatchGetSlotStatusRequest {
            current_block: 1001,
            btc_block: 100,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                },
            ],
        });

        let response = service.batch_get_slot_status(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Unlocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Unlocked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_get_slot_status_revert() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // First lock some slots at block 100
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 100,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                    revert_value: vec![5, 6, 7],
                    current_value: vec![8, 9, 10],
                    btc_txid: "txid1".to_string(),
                },
            ],
        });
        service.batch_lock_slot(request).await?;

        // Check status - should be reverted since block delta > 6
        let request = Request::new(BatchGetSlotStatusRequest {
            current_block: 1001,
            btc_block: 110,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                },
            ],
        });

        let response = service.batch_get_slot_status(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(response.get_ref().slots[0].revert_value, vec![4, 5, 6]);
        assert_eq!(response.get_ref().slots[0].current_value, vec![7, 8, 9]);
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(response.get_ref().slots[1].revert_value, vec![5, 6, 7]);
        assert_eq!(response.get_ref().slots[1].current_value, vec![8, 9, 10]);

        Ok(())
    }

    #[tokio::test]
    async fn test_get_slot_status_future_block() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Lock a slot for a future block
        let lock_request = Request::new(LockSlotRequest {
            locked_at_block: 1001,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });
        service.lock_slot(lock_request).await?;

        // Check status at block 1000 (before the lock's start_block)
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        // Should be unlocked because current_block < start_block
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Unlocked as i32
        );
        assert!(response.get_ref().revert_value.is_empty());
        assert!(response.get_ref().current_value.is_empty());

        // Now check at block 1001 (equal to the lock's start_block)
        let request = Request::new(GetSlotStatusRequest {
            current_block: 1001, // Current block equals locked_block
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(request).await?;
        // Should now be locked because current_block >= start_block
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Locked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_get_slot_status_future_block() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc.clone(), 6);

        // Lock slots for a future block
        let request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1001,
            btc_block: 100,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                    revert_value: vec![5, 6, 7],
                    current_value: vec![8, 9, 10],
                    btc_txid: "txid2".to_string(),
                },
            ],
        });
        service.batch_lock_slot(request).await?;

        // Check status at block 1000 (before the lock's start_block)
        let request = Request::new(BatchGetSlotStatusRequest {
            current_block: 1000,
            btc_block: 100,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                },
            ],
        });

        let response = service.batch_get_slot_status(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        // Both should be unlocked because current_block < start_block
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Unlocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Unlocked as i32
        );

        // Now check at block 1001 (equal to the lock's start_block)
        let request = Request::new(BatchGetSlotStatusRequest {
            current_block: 1001, // Current block equals locked_block
            btc_block: 100,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: "0x456".to_string(),
                    slot_index: vec![2, 3, 4],
                },
            ],
        });

        let response = service.batch_get_slot_status(request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        // Both should now be locked because current_block >= start_block
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Locked as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_slot_lock_flow() -> Result<(), Box<dyn std::error::Error>> {
        // Setup
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc, 6);

        // Test constants
        let contract_address = "0xabc123";
        let slot_a_index = vec![1, 2, 3];
        let slot_b_index = vec![4, 5, 6];
        let revert_value = vec![7, 8, 9];
        let current_value = vec![10, 11, 12];
        let btc_txid = "txid123";

        // Initial check that slots are unlocked
        let get_status_req = Request::new(BatchGetSlotStatusRequest {
            current_block: 2,
            btc_block: 101,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                },
            ],
        });

        let response = service.batch_get_slot_status(get_status_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Unlocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Unlocked as i32
        );

        // Lock both slots
        let lock_req = Request::new(BatchLockSlotRequest {
            locked_at_block: 3,
            btc_block: 101,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(lock_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        // Check status at block 2 (before lock block) - should be unlocked
        let get_status_req = Request::new(BatchGetSlotStatusRequest {
            current_block: 2,
            btc_block: 101,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                },
            ],
        });

        let response = service.batch_get_slot_status(get_status_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Unlocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Unlocked as i32
        );

        // Try to lock again - should be already locked
        let lock_req = Request::new(BatchLockSlotRequest {
            locked_at_block: 3,
            btc_block: 101,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(lock_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::AlreadyLocked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::AlreadyLocked as i32
        );

        // Check individual slot status at block 3 with high btc block - should be reverted
        let get_status_req = Request::new(BatchGetSlotStatusRequest {
            current_block: 3,
            btc_block: 221,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                },
            ],
        });

        let response = service.batch_get_slot_status(get_status_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Reverted as i32
        );

        // Repeat the previous check, the result should be the same
        let get_status_req = Request::new(BatchGetSlotStatusRequest {
            current_block: 3,
            btc_block: 221,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                },
            ],
        });

        let response = service.batch_get_slot_status(get_status_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Reverted as i32
        );

        // Lock slots again at new block height
        let lock_req = Request::new(BatchLockSlotRequest {
            locked_at_block: 4,
            btc_block: 221,
            slots: vec![
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
                sova_sentinel_proto::proto::SlotData {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                    revert_value: revert_value.clone(),
                    current_value: current_value.clone(),
                    btc_txid: btc_txid.to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(lock_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        // Check batch status at block 3 - should still be reverted
        let get_status_req = Request::new(BatchGetSlotStatusRequest {
            current_block: 3,
            btc_block: 221,
            slots: vec![
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_a_index.clone(),
                },
                sova_sentinel_proto::proto::SlotIdentifier {
                    contract_address: contract_address.to_string(),
                    slot_index: slot_b_index.clone(),
                },
            ],
        });

        let response = service.batch_get_slot_status(get_status_req).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Reverted as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Reverted as i32
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_slot_status_before_start_block() -> Result<(), Box<dyn std::error::Error>> {
        // Setup
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc, 6);

        // Lock a slot at block 1000
        let lock_request = Request::new(LockSlotRequest {
            locked_at_block: 1000, // Start block
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
            revert_value: vec![4, 5, 6],
            current_value: vec![7, 8, 9],
            btc_txid: "txid1".to_string(),
        });

        let response = service.lock_slot(lock_request).await?;
        assert_eq!(
            response.get_ref().status,
            lock_slot_response::Status::Locked as i32
        );

        // Check status at block 999 (before start_block)
        let status_request = Request::new(GetSlotStatusRequest {
            current_block: 999,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(status_request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Unlocked as i32,
            "Slot should be unlocked when queried before start_block"
        );

        // Check status at start_block
        let status_request = Request::new(GetSlotStatusRequest {
            current_block: 1000,
            btc_block: 100,
            contract_address: "0x123".to_string(),
            slot_index: vec![1, 2, 3],
        });

        let response = service.get_slot_status(status_request).await?;
        assert_eq!(
            response.get_ref().status,
            get_slot_status_response::Status::Locked as i32,
            "Slot should be locked when queried at start_block"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_batch_get_slot_status_before_start_block(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Setup
        let db = crate::db::Database::new(rusqlite::Connection::open_in_memory()?)?;
        let btc = MockBitcoinService::new();
        let service = SlotLockServiceImpl::new(db, btc, 6);

        // Lock two slots
        let lock_request = Request::new(BatchLockSlotRequest {
            locked_at_block: 1000,
            btc_block: 100,
            slots: vec![
                SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                    revert_value: vec![4, 5, 6],
                    current_value: vec![7, 8, 9],
                    btc_txid: "txid1".to_string(),
                },
                SlotData {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![4, 5, 6],
                    revert_value: vec![7, 8, 9],
                    current_value: vec![10, 11, 12],
                    btc_txid: "txid2".to_string(),
                },
            ],
        });

        let response = service.batch_lock_slot(lock_request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            slot_lock_status::Status::Locked as i32
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            slot_lock_status::Status::Locked as i32
        );

        // Check status at block 999 (before start_block)
        let status_request = Request::new(BatchGetSlotStatusRequest {
            current_block: 999,
            btc_block: 100,
            slots: vec![
                SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![4, 5, 6],
                },
            ],
        });

        let response = service.batch_get_slot_status(status_request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Unlocked as i32,
            "First slot should be unlocked when queried before start_block"
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Unlocked as i32,
            "Second slot should be unlocked when queried before start_block"
        );

        // Check status at start_block
        let status_request = Request::new(BatchGetSlotStatusRequest {
            current_block: 1000,
            btc_block: 100,
            slots: vec![
                SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![1, 2, 3],
                },
                SlotIdentifier {
                    contract_address: "0x123".to_string(),
                    slot_index: vec![4, 5, 6],
                },
            ],
        });

        let response = service.batch_get_slot_status(status_request).await?;
        assert_eq!(response.get_ref().slots.len(), 2);
        assert_eq!(
            response.get_ref().slots[0].status,
            get_slot_status_response::Status::Locked as i32,
            "First slot should be locked when queried at start_block"
        );
        assert_eq!(
            response.get_ref().slots[1].status,
            get_slot_status_response::Status::Locked as i32,
            "Second slot should be locked when queried at start_block"
        );

        Ok(())
    }
}
