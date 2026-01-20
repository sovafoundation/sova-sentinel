use tonic::transport::Channel;

use sova_sentinel_proto::proto::{
    slot_lock_service_client::SlotLockServiceClient, BatchGetSlotStatusRequest,
    BatchGetSlotStatusResponse, BatchLockSlotRequest, BatchLockSlotResponse,
    BatchUnlockSlotRequest, BatchUnlockSlotResponse, GetSlotStatusRequest, GetSlotStatusResponse,
    LockSlotRequest, LockSlotResponse, SlotData, SlotIdentifier,
};

pub struct SlotLockClient {
    client: SlotLockServiceClient<Channel>,
}

impl SlotLockClient {
    pub async fn connect(addr: String) -> Result<Self, tonic::transport::Error> {
        let client = SlotLockServiceClient::connect(addr).await?;
        Ok(Self { client })
    }

    pub async fn lock_slot(
        &mut self,
        locked_at_block: u64,
        btc_block: u64,
        slot: SlotData,
    ) -> Result<tonic::Response<LockSlotResponse>, tonic::Status> {
        let request = LockSlotRequest {
            locked_at_block,
            btc_block,
            contract_address: slot.contract_address,
            slot_index: slot.slot_index,
            revert_value: slot.revert_value,
            current_value: slot.current_value,
            btc_txid: slot.btc_txid,
        };

        self.client.lock_slot(request).await
    }

    pub async fn get_slot_status(
        &mut self,
        current_block: u64,
        btc_block: u64,
        contract_address: String,
        slot_index: Vec<u8>,
    ) -> Result<tonic::Response<GetSlotStatusResponse>, tonic::Status> {
        let request = GetSlotStatusRequest {
            current_block,
            btc_block,
            contract_address,
            slot_index,
        };

        self.client.get_slot_status(request).await
    }

    pub async fn batch_lock_slot(
        &mut self,
        locked_at_block: u64,
        btc_block: u64,
        slots: Vec<SlotData>,
    ) -> Result<tonic::Response<BatchLockSlotResponse>, tonic::Status> {
        let request = BatchLockSlotRequest {
            locked_at_block,
            btc_block,
            slots,
        };

        self.client.batch_lock_slot(request).await
    }

    pub async fn batch_get_slot_status(
        &mut self,
        current_block: u64,
        btc_block: u64,
        slots: Vec<SlotIdentifier>,
    ) -> Result<BatchGetSlotStatusResponse, Box<dyn std::error::Error>> {
        let response = self
            .client
            .batch_get_slot_status(BatchGetSlotStatusRequest {
                current_block,
                btc_block,
                slots,
            })
            .await?;

        Ok(response.into_inner())
    }

    pub async fn batch_unlock_slot(
        &mut self,
        current_block: u64,
        btc_block: u64,
        slots: Vec<SlotIdentifier>,
    ) -> Result<BatchUnlockSlotResponse, Box<dyn std::error::Error>> {
        let response = self
            .client
            .batch_unlock_slot(BatchUnlockSlotRequest {
                current_block,
                btc_block,
                slots,
            })
            .await?;

        Ok(response.into_inner())
    }
}
