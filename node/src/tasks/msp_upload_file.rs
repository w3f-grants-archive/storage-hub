use std::collections::HashMap;
use std::{cmp::max, str::FromStr, time::Duration};

use anyhow::anyhow;
use sc_network::PeerId;
use sc_tracing::tracing::*;
use shc_blockchain_service::types::{MspRespondStorageRequest, RespondStorageRequest, Tip};
use sp_core::{bounded_vec, H256};
use sp_runtime::AccountId32;

use crate::services::handler::StorageHubHandler;
use crate::tasks::{FileStorageT, MspForestStorageHandlerT};
use shc_actors_framework::event_bus::EventHandler;
use shc_blockchain_service::events::ProcessMspRespondStoringRequest;
use shc_blockchain_service::{commands::BlockchainServiceInterface, events::NewStorageRequest};
use shc_common::types::{
    AcceptedStorageRequestParameters, FileKey, FileMetadata, HashT, MspStorageRequestResponse,
    RejectedStorageRequestReason, StorageProofsMerkleTrieLayout, StorageProviderId,
};
use shc_file_manager::traits::{FileStorageWriteError, FileStorageWriteOutcome};
use shc_file_transfer_service::{
    commands::FileTransferServiceInterface, events::RemoteUploadRequest,
};
use shc_forest_manager::traits::ForestStorage;
use storage_hub_runtime::StorageDataUnit;

const LOG_TARGET: &str = "msp-upload-file-task";

const MAX_CONFIRM_STORING_REQUEST_TRY_COUNT: u32 = 3;

/// MSP Upload File Task: Handles the whole flow of a file being uploaded to a MSP, from
/// the MSP's perspective.
///
/// The flow is split into three parts, which are represented here as 3 handlers for 3
/// different events:
/// - [`NewStorageRequest`] event: The first part of the flow. It is triggered by a user
///   submitting a storage request to StorageHub. The MSP will check if it has enough
///   storage capacity to store the file and increase it if necessary (up to a maximum).
///   If the MSP does not have enough capacity still, it will reject the storage request.
///   It will register the user and file key in the registry of the File Transfer Service,
///   which handles incoming p2p upload requests. Finally, it will create a file in the
///   file storage so that it can write uploaded chunks as soon as possible.
/// - [`RemoteUploadRequest`] event: The second part of the flow. It is triggered by a
///   user sending a chunk of the file to the MSP. It checks the proof for the chunk
///   and if it is valid, stores it, until the whole file is stored. Finally the MSP will
///   queue a response to accept storing the file.
/// - [`ProcessMspRespondStoringRequest`] event: The third part of the flow. It is triggered
///   when there are new storage request(s) to respond to. The batch of storage requests
///   will be responded to in a single call to the FileSystem pallet `msp_respond_storage_requests_multiple_buckets` extrinsic
///   which will emit an event that describes the final result of the batch response (i.e. all accepted,
///   rejected and/or failed file keys). The MSP will then apply the necessary deltas to each one of the bucket's
///   forest storage to reflect the result.
pub struct MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    storage_hub_handler: StorageHubHandler<FL, FSH>,
    file_key_cleanup: Option<H256>,
}

impl<FL, FSH> Clone for MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    fn clone(&self) -> MspUploadFileTask<FL, FSH> {
        Self {
            storage_hub_handler: self.storage_hub_handler.clone(),
            file_key_cleanup: self.file_key_cleanup,
        }
    }
}

impl<FL, FSH> MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    pub fn new(storage_hub_handler: StorageHubHandler<FL, FSH>) -> Self {
        Self {
            storage_hub_handler,
            file_key_cleanup: None,
        }
    }
}

/// Handles the [`NewStorageRequest`] event.
///
/// This event is triggered by an on-chain event of a user submitting a storage request to StorageHub.
///
/// This task will:
/// - Check if the MSP has enough storage capacity to store the file and increase it if necessary (up to a maximum).
/// - Register the user and file key in the registry of the File Transfer Service, which handles incoming p2p
/// upload requests.
impl<FL, FSH> EventHandler<NewStorageRequest> for MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    async fn handle_event(&mut self, event: NewStorageRequest) -> anyhow::Result<()> {
        info!(
            target: LOG_TARGET,
            "Registering user peer for file_key {:?}, location {:?}, fingerprint {:?}",
            event.file_key,
            event.location,
            event.fingerprint
        );

        let result = self.handle_new_storage_request_event(event).await;
        if result.is_err() {
            if let Some(file_key) = &self.file_key_cleanup {
                self.unregister_file(*file_key).await?;
            }
        }
        result
    }
}

/// Handles the `RemoteUploadRequest` event.
///
/// This event is triggered by a user sending a chunk of the file to the MSP. It checks the proof
/// for the chunk and if it is valid, stores it, until the whole file is stored.
impl<FL, FSH> EventHandler<RemoteUploadRequest> for MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    async fn handle_event(&mut self, event: RemoteUploadRequest) -> anyhow::Result<()> {
        info!(target: LOG_TARGET, "Received remote upload request for file {:?} and peer {:?}", event.file_key, event.peer);

        let proven = match event
            .file_key_proof
            .proven::<StorageProofsMerkleTrieLayout>()
        {
            Ok(proven) => {
                if proven.len() != 1 {
                    Err(anyhow::anyhow!("Expected exactly one proven chunk."))
                } else {
                    Ok(proven[0].clone())
                }
            }
            Err(e) => Err(anyhow::anyhow!(
                "Failed to verify and get proven file key chunks: {:?}",
                e
            )),
        };

        let bucket_id = match self
            .storage_hub_handler
            .file_storage
            .read()
            .await
            .get_metadata(&event.file_key.into())
        {
            Ok(metadata) => match metadata {
                Some(metadata) => H256(metadata.bucket_id.try_into().unwrap()),
                None => {
                    let err_msg = format!("File does not exist for key {:?}. Maybe we forgot to unregister before deleting?", event.file_key);
                    error!(target: LOG_TARGET, err_msg);
                    return Err(anyhow!(err_msg));
                }
            },
            Err(e) => {
                let err_msg = format!("Failed to get file metadata: {:?}", e);
                error!(target: LOG_TARGET, err_msg);
                return Err(anyhow!(err_msg));
            }
        };

        // Reject storage request if the proof is invalid.
        let proven = match proven {
            Ok(proven) => proven,
            Err(e) => {
                warn!(target: LOG_TARGET, "{}", e);

                let call = storage_hub_runtime::RuntimeCall::FileSystem(
                    pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                        file_key_responses_input: bounded_vec![(
                            bucket_id,
                            MspStorageRequestResponse {
                                accept: None,
                                reject: Some(bounded_vec![(
                                    H256(event.file_key.into()),
                                    RejectedStorageRequestReason::ReceivedInvalidProof,
                                )])
                            }
                        )],
                    },
                );

                // Send extrinsic and wait for it to be included in the block.
                self.storage_hub_handler
                    .blockchain
                    .send_extrinsic(call, Tip::from(0))
                    .await?
                    .with_timeout(Duration::from_secs(60))
                    .watch_for_success(&self.storage_hub_handler.blockchain)
                    .await?;

                // Unregister the file.
                self.unregister_file(event.file_key.into()).await?;
                return Err(e);
            }
        };

        let mut write_file_storage = self.storage_hub_handler.file_storage.write().await;
        let write_chunk_result =
            write_file_storage.write_chunk(&event.file_key.into(), &proven.key, &proven.data);
        // Release the file storage write lock as soon as possible.
        drop(write_file_storage);

        match write_chunk_result {
            Ok(outcome) => match outcome {
                FileStorageWriteOutcome::FileComplete => {
                    self.on_file_complete(&event.file_key.into()).await?;
                }
                FileStorageWriteOutcome::FileIncomplete => {}
            },
            Err(error) => match error {
                FileStorageWriteError::FileChunkAlreadyExists => {
                    warn!(
                        target: LOG_TARGET,
                        "Received duplicate chunk with key: {:?}",
                        proven.key
                    );

                    // TODO: Consider informing this to the file transfer service so that it can handle reputation for this peer id.
                }
                FileStorageWriteError::FileDoesNotExist => {
                    let call = storage_hub_runtime::RuntimeCall::FileSystem(
                        pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                            file_key_responses_input: bounded_vec![(
                                bucket_id,
                                MspStorageRequestResponse {
                                    accept: None,
                                    reject: Some(bounded_vec![(
                                        H256(event.file_key.into()),
                                        RejectedStorageRequestReason::InternalError
                                    )])
                                }
                            )],
                        },
                    );

                    // Send extrinsic and wait for it to be included in the block.
                    self.storage_hub_handler
                        .blockchain
                        .send_extrinsic(call, Tip::from(0))
                        .await?
                        .with_timeout(Duration::from_secs(60))
                        .watch_for_success(&self.storage_hub_handler.blockchain)
                        .await?;

                    // Unregister the file.
                    self.unregister_file(event.file_key.into()).await?;

                    return Err(anyhow::anyhow!(format!("File does not exist for key {:?}. Maybe we forgot to unregister before deleting?", event.file_key)));
                }
                FileStorageWriteError::FailedToGetFileChunk
                | FileStorageWriteError::FailedToInsertFileChunk
                | FileStorageWriteError::FailedToDeleteChunk
                | FileStorageWriteError::FailedToPersistChanges
                | FileStorageWriteError::FailedToParseFileMetadata
                | FileStorageWriteError::FailedToParseFingerprint
                | FileStorageWriteError::FailedToReadStorage
                | FileStorageWriteError::FailedToUpdatePartialRoot
                | FileStorageWriteError::FailedToParsePartialRoot
                | FileStorageWriteError::FailedToGetStoredChunksCount => {
                    // This internal error should not happen.
                    let call = storage_hub_runtime::RuntimeCall::FileSystem(
                        pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                            file_key_responses_input: bounded_vec![(
                                bucket_id,
                                MspStorageRequestResponse {
                                    accept: None,
                                    reject: Some(bounded_vec![(
                                        H256(event.file_key.into()),
                                        RejectedStorageRequestReason::InternalError
                                    )])
                                }
                            )],
                        },
                    );

                    // Send extrinsic and wait for it to be included in the block.
                    self.storage_hub_handler
                        .blockchain
                        .send_extrinsic(call, Tip::from(0))
                        .await?
                        .with_timeout(Duration::from_secs(60))
                        .watch_for_success(&self.storage_hub_handler.blockchain)
                        .await?;

                    // Unregister the file.
                    self.unregister_file(event.file_key.into()).await?;

                    return Err(anyhow::anyhow!(format!(
                        "Internal trie read/write error {:?}:{:?}",
                        event.file_key, proven.key
                    )));
                }
                FileStorageWriteError::FingerprintAndStoredFileMismatch => {
                    // This should never happen, given that the first check in the handler is verifying the proof.
                    // This means that something is seriously wrong, so we error out the whole task.
                    let call = storage_hub_runtime::RuntimeCall::FileSystem(
                        pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                            file_key_responses_input: bounded_vec![(
                                bucket_id,
                                MspStorageRequestResponse {
                                    accept: None,
                                    reject: Some(bounded_vec![(
                                        H256(event.file_key.into()),
                                        RejectedStorageRequestReason::InternalError
                                    )])
                                }
                            )],
                        },
                    );

                    // Send extrinsic and wait for it to be included in the block.
                    self.storage_hub_handler
                        .blockchain
                        .send_extrinsic(call, Tip::from(0))
                        .await?
                        .with_timeout(Duration::from_secs(60))
                        .watch_for_success(&self.storage_hub_handler.blockchain)
                        .await?;

                    // Unregister the file.
                    self.unregister_file(event.file_key.into()).await?;

                    return Err(anyhow::anyhow!(format!(
                        "Invariant broken! This is a bug! Fingerprint and stored file mismatch for key {:?}.",
                        event.file_key
                    )));
                }
                FileStorageWriteError::FailedToConstructTrieIter => {
                    // This should never happen for a well constructed trie.
                    // This means that something is seriously wrong, so we error out the whole task.
                    let call = storage_hub_runtime::RuntimeCall::FileSystem(
                        pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                            file_key_responses_input: bounded_vec![(
                                bucket_id,
                                MspStorageRequestResponse {
                                    accept: None,
                                    reject: Some(bounded_vec![(
                                        H256(event.file_key.into()),
                                        RejectedStorageRequestReason::InternalError
                                    )])
                                }
                            )],
                        },
                    );

                    // Send extrinsic and wait for it to be included in the block.
                    self.storage_hub_handler
                        .blockchain
                        .send_extrinsic(call, Tip::from(0))
                        .await?
                        .with_timeout(Duration::from_secs(60))
                        .watch_for_success(&self.storage_hub_handler.blockchain)
                        .await?;

                    // Unregister the file.
                    self.unregister_file(event.file_key.into()).await?;

                    return Err(anyhow::anyhow!(format!(
                        "This is a bug! Failed to construct trie iter for key {:?}.",
                        event.file_key
                    )));
                }
            },
        }

        Ok(())
    }
}

/// Handles the `ProcessMspRespondStoringRequest` event.
///
/// Triggered when there are new storage request(s) to respond to. Normally, storage requests are
/// immidiately rejected if the MSP cannot store the file (e.g. not enough capacity). However, this event
/// is able to respond to storage requests that are either being accepted or rejected either way.
///
/// The MSP will call the `msp_respond_storage_requests_multiple_buckets` extrinsic on the FileSystem pallet to respond to the
/// storage requests.
impl<FL, FSH> EventHandler<ProcessMspRespondStoringRequest> for MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    async fn handle_event(&mut self, event: ProcessMspRespondStoringRequest) -> anyhow::Result<()> {
        info!(
            target: LOG_TARGET,
            "Processing ProcessMspRespondStoringRequest: {:?}",
            event.data.respond_storing_requests,
        );

        let forest_root_write_tx = match event.forest_root_write_tx.lock().await.take() {
            Some(tx) => tx,
            None => {
                let err_msg = "CRITICAL❗️❗️ This is a bug! Forest root write tx already taken. This is a critical bug. Please report it to the StorageHub team.";
                error!(target: LOG_TARGET, err_msg);
                return Err(anyhow!(err_msg));
            }
        };

        let own_provider_id = self
            .storage_hub_handler
            .blockchain
            .query_storage_provider_id(None)
            .await?;

        let own_msp_id = match own_provider_id {
            Some(StorageProviderId::MainStorageProvider(id)) => id,
            Some(StorageProviderId::BackupStorageProvider(_)) => {
                return Err(anyhow!("Current node account is a Backup Storage Provider. Expected a Main Storage Provider ID."));
            }
            None => {
                return Err(anyhow!("Failed to get own MSP ID."));
            }
        };

        let mut file_key_responses: HashMap<
            H256,
            (Vec<(H256, _)>, Vec<(H256, RejectedStorageRequestReason)>),
        > = HashMap::new();
        let read_file_storage = self.storage_hub_handler.file_storage.read().await;

        for respond in &event.data.respond_storing_requests {
            let bucket_id = match read_file_storage.get_metadata(&respond.file_key) {
                Ok(Some(metadata)) => H256(metadata.bucket_id.try_into().unwrap()),
                Ok(None) => {
                    error!(target: LOG_TARGET, "File does not exist for key {:?}. Maybe we forgot to unregister before deleting?", respond.file_key);
                    continue;
                }
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to get file metadata: {:?}", e);
                    continue;
                }
            };

            let entry = file_key_responses
                .entry(bucket_id)
                .or_insert_with(|| (Vec::new(), Vec::new()));

            match &respond.response {
                MspRespondStorageRequest::Accept => {
                    let chunks_to_prove = match self
                        .storage_hub_handler
                        .blockchain
                        .query_msp_confirm_chunks_to_prove_for_file(own_msp_id, respond.file_key)
                        .await
                    {
                        Ok(chunks) => chunks,
                        Err(e) => {
                            error!(target: LOG_TARGET, "Failed to get chunks to prove: {:?}", e);
                            continue;
                        }
                    };

                    let proof = match read_file_storage
                        .generate_proof(&respond.file_key, &chunks_to_prove)
                    {
                        Ok(p) => p,
                        Err(e) => {
                            error!(target: LOG_TARGET, "Failed to generate proof: {:?}", e);
                            continue;
                        }
                    };

                    entry.0.push((respond.file_key, proof));
                }
                MspRespondStorageRequest::Reject(reason) => {
                    entry.1.push((respond.file_key, reason.clone()));
                }
            }
        }

        drop(read_file_storage);

        let mut final_responses: HashMap<H256, MspStorageRequestResponse> = HashMap::new();

        for (bucket_id, (accepts, rejects)) in file_key_responses.iter_mut() {
            let fs = match self
                .storage_hub_handler
                .forest_storage_handler
                .get(&bucket_id.as_ref().to_vec())
                .await
            {
                Some(fs) => fs,
                None => {
                    error!(target: LOG_TARGET, "Failed to get forest storage for bucket {:?}", bucket_id);
                    continue;
                }
            };

            let file_keys: Vec<_> = accepts.iter().map(|(file_key, _)| *file_key).collect();

            let non_inclusion_forest_proof = match fs.read().await.generate_proof(file_keys) {
                Ok(proof) => proof,
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to generate non-inclusion forest proof: {:?}", e);
                    continue;
                }
            };

            let file_metadatas: Vec<_> = {
                let read_file_storage = self.storage_hub_handler.file_storage.read().await;
                accepts
                    .iter()
                    .filter_map(|(file_key, _)| {
                        read_file_storage.get_metadata(file_key).ok().flatten()
                    })
                    .collect()
            };

            if let Err(e) = fs.write().await.insert_files_metadata(&file_metadatas) {
                error!(target: LOG_TARGET, "Failed to insert file metadata: {:?}", e);
                continue;
            }

            let response = MspStorageRequestResponse {
                accept: if !accepts.is_empty() {
                    Some(AcceptedStorageRequestParameters {
                        file_keys_and_proofs: accepts
                            .clone()
                            .try_into()
                            .map_err(|_| anyhow!("Failed to convert accepts to bounded vec"))?,
                        non_inclusion_forest_proof: non_inclusion_forest_proof.proof,
                    })
                } else {
                    None
                },
                reject: if !rejects.is_empty() {
                    Some(
                        rejects
                            .clone()
                            .try_into()
                            .map_err(|_| anyhow!("Failed to convert rejects to bounded vec"))?,
                    )
                } else {
                    None
                },
            };

            final_responses.insert(*bucket_id, response);
        }

        let call = storage_hub_runtime::RuntimeCall::FileSystem(
            pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                file_key_responses_input: final_responses
                    .into_iter()
                    .collect::<Vec<_>>()
                    .try_into()
                    .map_err(|_| anyhow!("Failed to convert file key responses to bounded vec"))?,
            },
        );

        self.storage_hub_handler
            .blockchain
            .send_extrinsic(call, Tip::from(0))
            .await?
            .with_timeout(Duration::from_secs(60))
            .watch_for_success(&self.storage_hub_handler.blockchain)
            .await?;

        // Release the forest root write "lock" and finish the task.
        self.storage_hub_handler
            .blockchain
            .release_forest_root_write_lock(forest_root_write_tx)
            .await
    }
}

impl<FL, FSH> MspUploadFileTask<FL, FSH>
where
    FL: FileStorageT,
    FSH: MspForestStorageHandlerT,
{
    async fn handle_new_storage_request_event(
        &mut self,
        event: NewStorageRequest,
    ) -> anyhow::Result<()> {
        let own_provider_id = self
            .storage_hub_handler
            .blockchain
            .query_storage_provider_id(None)
            .await?;

        let own_msp_id = match own_provider_id {
            Some(id) => match id {
                StorageProviderId::MainStorageProvider(id) => id,
                StorageProviderId::BackupStorageProvider(_) => {
                    let err_msg = "Current node account is a Backup Storage Provider. Expected a Main Storage Provider ID.";
                    error!(target: LOG_TARGET, err_msg);
                    return Err(anyhow!(err_msg));
                }
            },
            None => {
                let err_msg = "Failed to get own MSP ID.";
                error!(target: LOG_TARGET, err_msg);
                return Err(anyhow!(err_msg));
            }
        };

        let msp_id_of_bucket_id = self
            .storage_hub_handler
            .blockchain
            .query_msp_id_of_bucket_id(event.bucket_id)
            .await
            .map_err(|e| {
                let err_msg = format!(
                    "Failed to query MSP ID of bucket ID {:?}\n Error: {:?}",
                    event.bucket_id, e
                );
                error!(target: LOG_TARGET, err_msg);
                anyhow!(err_msg)
            })?;

        if own_msp_id != msp_id_of_bucket_id {
            // Skip the file if the MSP ID of the bucket ID does not match the node's MSP ID.
            return Ok(());
        }

        // Construct file metadata.
        let metadata = FileMetadata {
            owner: <AccountId32 as AsRef<[u8]>>::as_ref(&event.who).to_vec(),
            bucket_id: event.bucket_id.as_ref().to_vec(),
            file_size: event.size as u64,
            fingerprint: event.fingerprint,
            location: event.location.to_vec(),
        };

        let available_capacity = self
            .storage_hub_handler
            .blockchain
            .query_available_storage_capacity(own_msp_id)
            .await
            .map_err(|e| {
                let err_msg = format!("Failed to query available storage capacity: {:?}", e);
                error!(
                    target: LOG_TARGET,
                    err_msg
                );
                anyhow::anyhow!(err_msg)
            })?;

        // Increase storage capacity if the available capacity is less than the file size.
        if available_capacity < event.size {
            warn!(
                target: LOG_TARGET,
                "Insufficient storage capacity to accept file: {:?}",
                event.file_key
            );

            let current_capacity = self
                .storage_hub_handler
                .blockchain
                .query_storage_provider_capacity(own_msp_id)
                .await
                .map_err(|e| {
                    let err_msg = format!("Failed to query storage provider capacity: {:?}", e);
                    error!(
                        target: LOG_TARGET,
                        err_msg
                    );
                    anyhow::anyhow!(err_msg)
                })?;

            let max_storage_capacity = self
                .storage_hub_handler
                .provider_config
                .max_storage_capacity;

            if max_storage_capacity == current_capacity {
                let err_msg = "Reached maximum storage capacity limit. Unable to add more more storage capacity.";
                warn!(
                    target: LOG_TARGET, err_msg
                );
                return Err(anyhow::anyhow!(err_msg));
            }

            let new_capacity = self.calculate_capacity(&event, current_capacity)?;

            let call = storage_hub_runtime::RuntimeCall::Providers(
                pallet_storage_providers::Call::change_capacity { new_capacity },
            );

            let earliest_change_capacity_block = self
                .storage_hub_handler
                .blockchain
                .query_earliest_change_capacity_block(own_msp_id)
                .await
                .map_err(|e| {
                    error!(
                        target: LOG_TARGET,
                        "Failed to query storage provider capacity: {:?}", e
                    );
                    anyhow::anyhow!("Failed to query storage provider capacity: {:?}", e)
                })?;

            // Wait for the earliest block where the capacity can be changed.
            self.storage_hub_handler
                .blockchain
                .wait_for_block(earliest_change_capacity_block)
                .await?;

            self.storage_hub_handler
                .blockchain
                .send_extrinsic(call, Tip::from(0))
                .await?
                .with_timeout(Duration::from_secs(60))
                .watch_for_success(&self.storage_hub_handler.blockchain)
                .await?;

            info!(
                target: LOG_TARGET,
                "Increased storage capacity to {:?} bytes",
                new_capacity
            );

            let available_capacity = self
                .storage_hub_handler
                .blockchain
                .query_available_storage_capacity(own_msp_id)
                .await
                .map_err(|e| {
                    error!(
                        target: LOG_TARGET,
                        "Failed to query available storage capacity: {:?}", e
                    );
                    anyhow::anyhow!("Failed to query available storage capacity: {:?}", e)
                })?;

            // Reject storage request if the new available capacity is still less than the file size.
            if available_capacity < event.size {
                let err_msg = "Increased storage capacity is still insufficient to volunteer for file. Rejecting storage request.";
                warn!(
                    target: LOG_TARGET, "{}", err_msg
                );

                // Build extrinsic.
                let call = storage_hub_runtime::RuntimeCall::FileSystem(
                    pallet_file_system::Call::msp_respond_storage_requests_multiple_buckets {
                        file_key_responses_input: bounded_vec![(
                            H256(metadata.bucket_id.try_into().map_err(|e| {
                                let err_msg =
                                    format!("Failed to convert bucket ID to [u8; 32]: {:?}", e);
                                error!(
                                    target: LOG_TARGET,
                                    err_msg
                                );
                                anyhow::anyhow!(err_msg)
                            })?),
                            MspStorageRequestResponse {
                                accept: None,
                                reject: Some(bounded_vec![(
                                    H256(event.file_key.into()),
                                    RejectedStorageRequestReason::ReachedMaximumCapacity,
                                )])
                            }
                        )],
                    },
                );

                // Send extrinsic and wait for it to be included in the block.
                self.storage_hub_handler
                    .blockchain
                    .send_extrinsic(call, Tip::from(0))
                    .await?
                    .with_timeout(Duration::from_secs(60))
                    .watch_for_success(&self.storage_hub_handler.blockchain)
                    .await?;

                return Err(anyhow::anyhow!(err_msg));
            }
        }

        // Get the file key.
        let file_key: FileKey = metadata
            .file_key::<HashT<StorageProofsMerkleTrieLayout>>()
            .as_ref()
            .try_into()?;

        self.file_key_cleanup = Some(file_key.into());

        // Register the file for upload in the file transfer service.
        for peer_id in event.user_peer_ids.iter() {
            let peer_id = match std::str::from_utf8(&peer_id.as_slice()) {
                Ok(str_slice) => PeerId::from_str(str_slice).map_err(|e| {
                    error!(target: LOG_TARGET, "Failed to convert peer ID to PeerId: {}", e);
                    e
                })?,
                Err(e) => return Err(anyhow!("Failed to convert peer ID to a string: {}", e)),
            };
            self.storage_hub_handler
                .file_transfer
                .register_new_file_peer(peer_id, file_key)
                .await
                .map_err(|e| anyhow!("Failed to register new file peer: {:?}", e))?;
        }

        // Create file in file storage so we can write uploaded chunks as soon as possible.
        let mut write_file_storage = self.storage_hub_handler.file_storage.write().await;
        write_file_storage
            .insert_file(
                metadata.file_key::<HashT<StorageProofsMerkleTrieLayout>>(),
                metadata,
            )
            .map_err(|e| anyhow!("Failed to insert file in file storage: {:?}", e))?;
        drop(write_file_storage);

        Ok(())
    }

    /// Calculate the new capacity after adding the required capacity for the file.
    ///
    /// The new storage capacity will be increased by the jump capacity until it reaches the
    /// `max_storage_capacity`.
    ///
    /// The `max_storage_capacity` is returned if the new capacity exceeds it.
    fn calculate_capacity(
        &mut self,
        event: &NewStorageRequest,
        current_capacity: StorageDataUnit,
    ) -> Result<StorageDataUnit, anyhow::Error> {
        let jump_capacity = self.storage_hub_handler.provider_config.jump_capacity;
        let jumps_needed = (event.size + jump_capacity - 1) / jump_capacity;
        let jumps = max(jumps_needed, 1);
        let bytes_to_add = jumps * jump_capacity;
        let required_capacity = current_capacity.checked_add(bytes_to_add).ok_or_else(|| {
            anyhow::anyhow!(
                "Reached maximum storage capacity limit. Skipping volunteering for file."
            )
        })?;

        let max_storage_capacity = self
            .storage_hub_handler
            .provider_config
            .max_storage_capacity;

        let new_capacity = std::cmp::min(required_capacity, max_storage_capacity);

        Ok(new_capacity)
    }

    async fn unregister_file(&self, file_key: H256) -> anyhow::Result<()> {
        warn!(target: LOG_TARGET, "Unregistering file {:?}", file_key);

        // Unregister the file from the file transfer service.
        // The error is ignored, as the file might already be unregistered.
        let _ = self
            .storage_hub_handler
            .file_transfer
            .unregister_file(file_key.as_ref().into())
            .await;

        // Delete the file from the file storage.
        let mut write_file_storage = self.storage_hub_handler.file_storage.write().await;

        // TODO: Handle error
        let _ = write_file_storage.delete_file(&file_key);

        Ok(())
    }

    async fn on_file_complete(&self, file_key: &H256) -> anyhow::Result<()> {
        info!(target: LOG_TARGET, "File upload complete ({:?})", file_key);

        // Unregister the file from the file transfer service.
        self.storage_hub_handler
            .file_transfer
            .unregister_file((*file_key).into())
            .await
            .map_err(|e| anyhow!("File is not registered. This should not happen!: {:?}", e))?;

        // Queue a request to confirm the storing of the file.
        self.storage_hub_handler
            .blockchain
            .queue_msp_respond_storage_request(RespondStorageRequest::new(
                *file_key,
                MspRespondStorageRequest::Accept,
            ))
            .await?;

        Ok(())
    }
}