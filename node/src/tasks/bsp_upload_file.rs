use std::{path::Path, time::Duration};

use anyhow::anyhow;
use forest_manager::traits::ForestStorage;
use sc_network::PeerId;
use sc_tracing::tracing::*;
use shc_common::types::{FileKey, FileMetadata, HasherOutT};

use file_manager::traits::{FileStorage, FileStorageWriteError, FileStorageWriteOutcome};
use sp_trie::TrieLayout;
use storage_hub_infra::event_bus::EventHandler;
use tokio::{fs::File, io::AsyncWriteExt};

use crate::services::{
    blockchain::{commands::BlockchainServiceInterface, events::NewStorageRequest},
    file_transfer::{commands::FileTransferServiceInterface, events::RemoteUploadRequest},
    handler::StorageHubHandler,
};

const LOG_TARGET: &str = "bsp-upload-file-task";

/// BSP Upload File Task: Handles the whole flow of a file being uploaded to a BSP, from
/// the BSP's perspective.
///
/// The flow is split into two parts, which are represented here as two handlers for two
/// different events:
/// - `NewStorageRequest` event: The first part of the flow. It is triggered by an
///   on-chain event of a user submitting a storage request to StorageHub. It responds
///   by sending a volunteer transaction and registering the interest of this BSP in
///   receiving the file.
/// - `RemoteUploadRequest` event: The second part of the flow. It is triggered by a
///   user sending a chunk of the file to the BSP. It checks the proof for the chunk
///   and if it is valid, stores it, until the whole file is stored.
pub struct BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout,
    FL: Send + Sync + FileStorage<T>,
    FS: Send + Sync + ForestStorage<T>,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    storage_hub_handler: StorageHubHandler<T, FL, FS>,
    file_key_cleanup: Option<HasherOutT<T>>,
}

impl<T, FL, FS> Clone for BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout,
    FL: Send + Sync + FileStorage<T>,
    FS: Send + Sync + ForestStorage<T>,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    fn clone(&self) -> BspUploadFileTask<T, FL, FS> {
        Self {
            storage_hub_handler: self.storage_hub_handler.clone(),
            file_key_cleanup: self.file_key_cleanup,
        }
    }
}

impl<T, FL, FS> BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout,
    FL: Send + Sync + FileStorage<T>,
    FS: Send + Sync + ForestStorage<T>,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    pub fn new(storage_hub_handler: StorageHubHandler<T, FL, FS>) -> Self {
        Self {
            storage_hub_handler,
            file_key_cleanup: None,
        }
    }
}

/// Handles the `NewStorageRequest` event.
///
/// This event is triggered by an on-chain event of a user submitting a storage request to StorageHub.
/// It responds by sending a volunteer transaction and registering the interest of this BSP in
/// receiving the file. This task optimistically assumes the transaction will succeed, and registers
/// the user and file key in the registry of the File Transfer Service, which handles incoming p2p
/// upload requests.
impl<T, FL, FS> EventHandler<NewStorageRequest> for BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout + Send + Sync + 'static,
    FL: FileStorage<T> + Send + Sync,
    FS: ForestStorage<T> + Send + Sync + 'static,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    async fn handle_event(&mut self, event: NewStorageRequest) -> anyhow::Result<()> {
        info!(
            target: LOG_TARGET,
            "Initiating BSP volunteer for location: {:?}, fingerprint: {:?}",
            event.location,
            event.fingerprint
        );

        let result = self.handle_new_storage_request_event(event).await;
        if result.is_err() {
            if let Some(file_key) = &self.file_key_cleanup {
                self.unvolunteer_file(*file_key).await?;
            }
        }
        result
    }
}

/// Handles the `RemoteUploadRequest` event.
///
/// This event is triggered by a user sending a chunk of the file to the BSP. It checks the proof
/// for the chunk and if it is valid, stores it, until the whole file is stored.
impl<T, FL, FS> EventHandler<RemoteUploadRequest> for BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout + Send + Sync + 'static,
    FL: FileStorage<T> + Send + Sync,
    FS: ForestStorage<T> + Send + Sync + 'static,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    async fn handle_event(&mut self, event: RemoteUploadRequest) -> anyhow::Result<()> {
        let file_key: HasherOutT<T> = TryFrom::try_from(*event.file_key.as_ref())
            .map_err(|_| anyhow::anyhow!("File key and HasherOutT mismatch!"))?;

        let proven = event
            .chunk_with_proof
            .proven
            .first()
            .ok_or_else(|| anyhow::anyhow!("Proven should contain one element; qed"))?;

        if !event.chunk_with_proof.verify() {
            // Unvolunteer the file.
            self.unvolunteer_file(file_key).await?;

            return Err(anyhow::anyhow!(format!(
                "Received invalid proof for chunk: {} (file: {:?}))",
                proven.key, event.file_key
            )));
        }

        let mut write_file_storage = self.storage_hub_handler.file_storage.write().await;
        let write_chunk_result =
            write_file_storage.write_chunk(&file_key, &proven.key, &proven.data);
        // Release the file storage write lock as soon as possible.
        drop(write_file_storage);

        match write_chunk_result {
            Ok(outcome) => match outcome {
                FileStorageWriteOutcome::FileComplete => self.on_file_complete(&file_key).await,
                FileStorageWriteOutcome::FileIncomplete => {}
            },
            Err(error) => match error {
                FileStorageWriteError::FileChunkAlreadyExists => {
                    warn!(
                        target: LOG_TARGET,
                        "Received duplicate chunk with key: {}",
                        proven.key
                    );

                    // TODO: Consider informing this to the file transfer service so that it can handle reputation for this peer id.
                }
                FileStorageWriteError::FileDoesNotExist => {
                    // Unvolunteer the file.
                    self.unvolunteer_file(file_key).await?;

                    return Err(anyhow::anyhow!(format!("File does not exist for key {:?}. Maybe we forgot to unregister before deleting?", event.file_key)));
                }
                FileStorageWriteError::FailedToGetFileChunk
                | FileStorageWriteError::FailedToInsertFileChunk => {
                    // This internal error should not happen.

                    // Unvolunteer the file.
                    self.unvolunteer_file(file_key).await?;

                    return Err(anyhow::anyhow!(format!(
                        "Internal trie read/write error {:?}:{}",
                        event.file_key, proven.key
                    )));
                }
                FileStorageWriteError::FingerprintAndStoredFileMismatch => {
                    // This should never happen, given that the first check in the handler is verifying the proof.
                    // This means that something is seriously wrong, so we error out the whole task.

                    // Unvolunteer the file.
                    self.unvolunteer_file(file_key).await?;

                    return Err(anyhow::anyhow!(format!(
                        "Invariant broken! This is a bug! Fingerprint and stored file mismatch for key {:?}.",
                        event.file_key
                    )));
                }
            },
        }

        Ok(())
    }
}

impl<T, FL, FS> BspUploadFileTask<T, FL, FS>
where
    T: TrieLayout,
    FL: Send + Sync + FileStorage<T>,
    FS: Send + Sync + ForestStorage<T>,
    HasherOutT<T>: TryFrom<[u8; 32]>,
{
    async fn handle_new_storage_request_event(
        &mut self,
        event: NewStorageRequest,
    ) -> anyhow::Result<()>
    where
        HasherOutT<T>: TryFrom<[u8; 32]>,
    {
        let fingerprint: [u8; 32] = event
            .fingerprint
            .as_ref()
            .try_into()
            .expect("Fingerprint should be 32 bytes; qed");

        // Construct file metadata.
        let metadata = FileMetadata {
            owner: event.who.to_string(),
            size: event.size as u64,
            fingerprint: event.fingerprint,
            location: event.location.to_vec(),
        };

        // Get the file key.
        let file_key: FileKey = metadata
            .key::<<T as TrieLayout>::Hash>()
            .as_ref()
            .try_into()?;

        let file_key_hash: HasherOutT<T> = TryFrom::<[u8; 32]>::try_from(*file_key.as_ref())
            .map_err(|_| anyhow::anyhow!("File key and HasherOutT mismatch!"))?;
        self.file_key_cleanup = Some(file_key_hash);

        // Optimistically register the file for upload in the file transfer service.
        // This solves the race condition between the user and the BSP, where the user could react faster
        // to the BSP volunteering than the BSP, and therefore initiate a new upload request before the
        // BSP has registered the file and peer ID in the file transfer service.
        for peer_id in event.user_peer_ids.iter() {
            let peer_id = PeerId::from_bytes(peer_id.as_slice())
                .map_err(|_| anyhow!("PeerId should be valid; qed"))?;
            self.storage_hub_handler
                .file_transfer
                .register_new_file_peer(peer_id, file_key)
                .await
                .map_err(|_| anyhow!("Failed to register peer file."))?;
        }

        // Build extrinsic.
        let call =
            storage_hub_runtime::RuntimeCall::FileSystem(pallet_file_system::Call::bsp_volunteer {
                location: event.location.clone(),
                fingerprint: fingerprint.into(),
            });

        self.storage_hub_handler
            .blockchain
            .send_extrinsic(call)
            .await?
            .with_timeout(Duration::from_secs(60))
            .watch_for_success(&self.storage_hub_handler.blockchain)
            .await?;

        Ok(())
    }

    async fn unvolunteer_file(&self, file_key: HasherOutT<T>) -> anyhow::Result<()> {
        // Unregister the file from the file transfer service.
        // The error is ignored, as the file might already be unregistered.
        let _ = self
            .storage_hub_handler
            .file_transfer
            .unregister_file(file_key.as_ref().into())
            .await;

        // Delete the file from the file storage.
        let mut write_file_storage = self.storage_hub_handler.file_storage.write().await;
        write_file_storage.delete_file(&file_key);

        // TODO: Send transaction to runtime to unvolunteer the file.

        Ok(())
    }

    async fn on_file_complete(&self, file_key: &HasherOutT<T>) {
        info!(target: LOG_TARGET, "File upload complete ({:?})", file_key);

        // Unregister the file from the file transfer service.
        self.storage_hub_handler
            .file_transfer
            .unregister_file(file_key.as_ref().into())
            .await
            .expect("File is not registered. This should not happen!");

        // Get the metadata for the file.
        let read_file_storage = self.storage_hub_handler.file_storage.read().await;
        let metadata = read_file_storage
            .get_metadata(file_key)
            .expect("File metadata not found");
        // Release the file storage read lock as soon as possible.
        drop(read_file_storage);

        // Save [`FileMetadata`] of the newly stored file in the forest storage.
        let mut write_forest_storage = self.storage_hub_handler.forest_storage.write().await;
        let file_key = write_forest_storage
            .insert_metadata(&metadata)
            .expect("Failed to insert metadata.");
        // Since we are done writing but need to generate a proof we choose to downgrade the lock to
        // a read lock.
        let read_forest_storage = write_forest_storage.downgrade();
        let _forest_proof = read_forest_storage
            .generate_proof(vec![file_key])
            .expect("Failed to generate forest proof.");
        // Release the forest storage read lock.
        drop(read_forest_storage);

        // TODO: send the proof for the new file to the runtime

        // TODO: move this under an RPC call
        let file_path = Path::new("./storage/").join(
            String::from_utf8(metadata.location.clone())
                .expect("File location should be an utf8 string"),
        );
        info!("Saving file to: {:?}", file_path);
        let mut file = File::create(file_path)
            .await
            .expect("Failed to open file for writing.");

        let read_file_storage = self.storage_hub_handler.file_storage.read().await;
        for chunk_id in 0..metadata.chunk_count() {
            let chunk = read_file_storage
                .get_chunk(&file_key, &chunk_id)
                .expect("Chunk not found in storage.");
            file.write_all(&chunk)
                .await
                .expect("Failed to write file chunk.");
        }
        drop(read_file_storage);
    }
}