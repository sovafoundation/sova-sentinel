use sova_sentinel_client::SlotLockClient;
use sova_sentinel_proto::proto::{SlotData, SlotIdentifier};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SlotLockClient::connect(String::from("http://[::1]:50051")).await?;

    // See protobuf definitions for request and response payloads
    // proto/src/proto/slot_lock.proto

    let address_1 = "0x1D1479C185d32EB90533a08b36B3CFa5F84A0E".to_string();
    let address_2 = "0x2D1479C185d32EB90533a08b36B3CFa5F85B0F".to_string();

    // Convert slot indices to bytes (big-endian)
    let slot_index_1 = 100u64.to_be_bytes().to_vec();
    let slot_index_2 = 101u64.to_be_bytes().to_vec();
    let slot_index_3 = 102u64.to_be_bytes().to_vec();

    let revert_bytes = vec![1, 2, 3];
    let current_bytes = vec![4, 5, 6];
    let btc_txid = "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16".to_string();
    let sova_block = 10;
    let btc_block = 99;

    // Example: Get slot status
    let response_status = client
        .get_slot_status(
            sova_block,
            btc_block,
            address_1.clone(),
            slot_index_1.clone(),
        )
        .await?;
    let status = response_status.into_inner();
    println!("Slot Status: {:?}", status);

    // Example: Lock a slot
    let slot = SlotData {
        contract_address: address_1.clone(),
        slot_index: slot_index_1.clone(),
        revert_value: revert_bytes.clone(),
        current_value: current_bytes.clone(),
        btc_txid: btc_txid.clone(),
    };
    let response_lock = client.lock_slot(sova_block, btc_block, slot).await?;

    let lock = response_lock.into_inner();
    println!("Lock response: {:?}", lock);

    // Example: Get slot status again
    let response_status2 = client
        .get_slot_status(
            sova_block,
            btc_block,
            address_1.clone(),
            slot_index_1.clone(),
        )
        .await?;

    let status2 = response_status2.into_inner();
    println!("Slot Status: {:?}", status2);

    // Sova blocks
    let start_block = 100; // Block when locking
    let end_block = 105; // Block when unlocking

    // Example: Batch operations
    let slots = vec![
        SlotData {
            contract_address: address_1.clone(),
            slot_index: slot_index_1.clone(),
            revert_value: revert_bytes.clone(),
            current_value: current_bytes.clone(),
            btc_txid: "txid1".to_string(),
        },
        SlotData {
            contract_address: address_2.clone(),
            slot_index: slot_index_2.clone(),
            revert_value: vec![7, 8, 9],
            current_value: vec![10, 11, 12],
            btc_txid: "txid2".to_string(),
        },
    ];

    // 1. Check initial status
    let status_slots = vec![
        SlotIdentifier {
            contract_address: address_1.clone(),
            slot_index: slot_index_1.clone(),
        },
        SlotIdentifier {
            contract_address: address_2.clone(),
            slot_index: slot_index_2.clone(),
        },
    ];

    let status_response = client
        .batch_get_slot_status(start_block, btc_block, status_slots.clone())
        .await?;
    println!("Initial Status: {:?}", status_response);

    // 2. Lock both slots at start_block
    let response = client
        .batch_lock_slot(start_block, btc_block, slots.clone())
        .await?;
    println!("Batch lock response: {:?}", response);

    // 3. Check status after locking
    let status_response = client
        .batch_get_slot_status(start_block, btc_block, status_slots.clone())
        .await?;
    println!("Status After Lock: {:?}", status_response);

    // 4. Development: Force unlock slots at end_block
    let unlock_response = client
        .batch_unlock_slot(end_block, btc_block, status_slots.clone())
        .await?;
    println!("Unlock Response: {:?}", unlock_response);

    // 5. Verify slots are unlocked
    let status_response = client
        .batch_get_slot_status(end_block, btc_block, status_slots)
        .await?;
    println!("Final Status: {:?}", status_response);

    // Batch lock slots with multiple contract addresses
    let slots = vec![
        SlotData {
            contract_address: address_1.clone(),
            slot_index: slot_index_2.clone(),
            revert_value: revert_bytes.clone(),
            current_value: current_bytes.clone(),
            btc_txid: "txid3".to_string(),
        },
        SlotData {
            contract_address: address_2.clone(),
            slot_index: slot_index_3.clone(),
            revert_value: vec![7, 8, 9],
            current_value: vec![10, 11, 12],
            btc_txid: "txid4".to_string(),
        },
    ];

    let response = client
        .batch_lock_slot(start_block, btc_block, slots)
        .await?;

    println!("Batch lock response: {:?}", response);

    Ok(())
}
