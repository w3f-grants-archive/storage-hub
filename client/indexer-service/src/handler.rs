use diesel_async::AsyncConnection;
use futures::prelude::*;
use log::{error, info};
use shc_common::types::StorageProviderId;
use std::sync::Arc;
use thiserror::Error;

use sc_client_api::{BlockBackend, BlockchainEvents};
use sp_core::H256;
use sp_runtime::traits::Header;

use shc_actors_framework::actor::{Actor, ActorEventLoop};
use shc_common::blockchain_utils::EventsRetrievalError;
use shc_common::{
    blockchain_utils::get_events_at_block,
    types::{BlockNumber, ParachainClient},
};
use shc_indexer_db::{models::*, DbConnection, DbPool};
use storage_hub_runtime::RuntimeEvent;

pub(crate) const LOG_TARGET: &str = "indexer-service";

// Since the indexed data should be used directly from the database,
// we don't need to implement commands.
#[derive(Debug)]
pub enum IndexerServiceCommand {}

// The IndexerService actor
pub struct IndexerService {
    client: Arc<ParachainClient>,
    db_pool: DbPool,
}

// Implement the Actor trait for IndexerService
impl Actor for IndexerService {
    type Message = IndexerServiceCommand;
    type EventLoop = IndexerServiceEventLoop;
    type EventBusProvider = (); // We're not using an event bus for now

    fn handle_message(
        &mut self,
        message: Self::Message,
    ) -> impl std::future::Future<Output = ()> + Send {
        async move {
            match message {
                // No commands for now
            }
        }
    }

    fn get_event_bus_provider(&self) -> &Self::EventBusProvider {
        &()
    }
}

// Implement methods for IndexerService
impl IndexerService {
    pub fn new(client: Arc<ParachainClient>, db_pool: DbPool) -> Self {
        Self { client, db_pool }
    }

    async fn handle_finality_notification<Block>(
        &mut self,
        notification: sc_client_api::FinalityNotification<Block>,
    ) -> Result<(), HandleFinalityNotificationError>
    where
        Block: sp_runtime::traits::Block,
        Block::Header: Header<Number = BlockNumber>,
    {
        let finalized_block_hash = notification.hash;
        let finalized_block_number = *notification.header.number();

        info!(target: LOG_TARGET, "Finality notification (#{}): {}", finalized_block_number, finalized_block_hash);

        let mut db_conn = self.db_pool.get().await?;

        let service_state = ServiceState::get(&mut db_conn).await?;

        for block_number in
            (service_state.last_processed_block as BlockNumber + 1)..=finalized_block_number
        {
            let block_hash = self
                .client
                .block_hash(block_number)?
                .ok_or(HandleFinalityNotificationError::BlockHashNotFound)?;
            self.index_block(&mut db_conn, block_number as BlockNumber, block_hash)
                .await?;
        }

        Ok(())
    }

    async fn index_block<'a, 'b: 'a>(
        &'b self,
        conn: &mut DbConnection<'a>,
        block_number: BlockNumber,
        block_hash: H256,
    ) -> Result<(), IndexBlockError> {
        info!(target: LOG_TARGET, "Indexing block #{}: {}", block_number, block_hash);

        let block_events = get_events_at_block(&self.client, &block_hash)?;

        conn.transaction::<(), IndexBlockError, _>(move |conn| {
            Box::pin(async move {
                ServiceState::update(conn, block_number as i64).await?;

                for ev in block_events {
                    self.index_event(conn, &ev.event).await?;
                }

                Ok(())
            })
        })
        .await?;

        Ok(())
    }

    async fn index_event<'a, 'b: 'a>(
        &'b self,
        conn: &mut DbConnection<'a>,
        event: &RuntimeEvent,
    ) -> Result<(), diesel::result::Error> {
        match event {
            RuntimeEvent::BucketNfts(event) => self.index_bucket_nfts_event(conn, event).await?,
            RuntimeEvent::FileSystem(event) => self.index_file_system_event(conn, event).await?,
            RuntimeEvent::PaymentStreams(event) => {
                self.index_payment_streams_event(conn, event).await?
            }
            RuntimeEvent::ProofsDealer(event) => {
                self.index_proofs_dealer_event(conn, event).await?
            }
            RuntimeEvent::Providers(event) => self.index_providers_event(conn, event).await?,
            RuntimeEvent::Randomness(event) => self.index_randomness_event(conn, event).await?,
            // Runtime events that we're not interested in.
            // We add them here instead of directly matching (_ => {})
            // to ensure the compiler will let us know to treat future events when added.
            RuntimeEvent::System(_) => {}
            RuntimeEvent::ParachainSystem(_) => {}
            RuntimeEvent::Balances(_) => {}
            RuntimeEvent::TransactionPayment(_) => {}
            RuntimeEvent::Sudo(_) => {}
            RuntimeEvent::CollatorSelection(_) => {}
            RuntimeEvent::Session(_) => {}
            RuntimeEvent::XcmpQueue(_) => {}
            RuntimeEvent::PolkadotXcm(_) => {}
            RuntimeEvent::CumulusXcm(_) => {}
            RuntimeEvent::MessageQueue(_) => {}
            RuntimeEvent::Nfts(_) => {}
            RuntimeEvent::Parameters(_) => {}
        }

        Ok(())
    }

    async fn index_bucket_nfts_event<'a, 'b: 'a>(
        &'b self,
        _conn: &mut DbConnection<'a>,
        event: &pallet_bucket_nfts::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_bucket_nfts::Event::AccessShared { .. } => {}
            pallet_bucket_nfts::Event::ItemReadAccessUpdated { .. } => {}
            pallet_bucket_nfts::Event::ItemBurned { .. } => {}
            pallet_bucket_nfts::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }

    async fn index_file_system_event<'a, 'b: 'a>(
        &'b self,
        conn: &mut DbConnection<'a>,
        event: &pallet_file_system::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_file_system::Event::NewBucket {
                who,
                msp_id,
                bucket_id,
                name,
                collection_id,
                private,
            } => {
                let msp = Msp::get_by_onchain_msp_id(conn, msp_id.to_string()).await?;
                Bucket::create(
                    conn,
                    msp.id,
                    who.to_string(),
                    bucket_id.to_string(),
                    name.to_vec(),
                    collection_id.map(|id| id.to_string()),
                    *private,
                )
                .await?;
            }
            pallet_file_system::Event::MoveBucketAccepted { msp_id, bucket_id } => {
                let msp = Msp::get_by_onchain_msp_id(conn, msp_id.to_string()).await?;
                Bucket::update_msp(conn, bucket_id.to_string(), msp.id).await?;
            }
            pallet_file_system::Event::BucketPrivacyUpdated {
                who,
                bucket_id,
                collection_id,
                private,
            } => {
                Bucket::update_privacy(
                    conn,
                    who.to_string(),
                    bucket_id.to_string(),
                    collection_id.map(|id| id.to_string()),
                    *private,
                )
                .await?;
            }
            pallet_file_system::Event::BspConfirmStoppedStoring { .. } => {}
            pallet_file_system::Event::BspConfirmedStoring { .. } => {}
            pallet_file_system::Event::MspRespondedToStorageRequests { .. } => {}
            pallet_file_system::Event::NewStorageRequest { .. } => {}
            pallet_file_system::Event::MoveBucketRequested { .. } => {}
            pallet_file_system::Event::NewCollectionAndAssociation { .. } => {}
            pallet_file_system::Event::AcceptedBspVolunteer { .. } => {}
            pallet_file_system::Event::StorageRequestFulfilled { .. } => {}
            pallet_file_system::Event::StorageRequestExpired { .. } => {}
            pallet_file_system::Event::StorageRequestRevoked { .. } => {}
            pallet_file_system::Event::BspRequestedToStopStoring { .. } => {}
            pallet_file_system::Event::PriorityChallengeForFileDeletionQueued { .. } => {}
            pallet_file_system::Event::SpStopStoringInsolventUser { .. } => {}
            pallet_file_system::Event::FailedToQueuePriorityChallenge { .. } => {}
            pallet_file_system::Event::FileDeletionRequest { .. } => {}
            pallet_file_system::Event::ProofSubmittedForPendingFileDeletionRequest { .. } => {}
            pallet_file_system::Event::BspChallengeCycleInitialised { .. } => {}
            pallet_file_system::Event::MoveBucketRequestExpired { .. } => {}
            pallet_file_system::Event::MoveBucketRejected { .. } => {}
            pallet_file_system::Event::DataServerRegisteredForMoveBucket { .. } => {}
            pallet_file_system::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }

    async fn index_payment_streams_event<'a, 'b: 'a>(
        &'b self,
        conn: &mut DbConnection<'a>,
        event: &pallet_payment_streams::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_payment_streams::Event::DynamicRatePaymentStreamCreated {
                provider_id,
                user_account,
                amount_provided: _amount_provided,
            } => {
                PaymentStream::create(conn, provider_id.to_string(), user_account.to_string())
                    .await?;
            }
            pallet_payment_streams::Event::DynamicRatePaymentStreamUpdated { .. } => {
                // TODO: Currently we are not treating the info of dynamic rate update
            }
            pallet_payment_streams::Event::DynamicRatePaymentStreamDeleted { .. } => {}
            pallet_payment_streams::Event::FixedRatePaymentStreamCreated {
                provider_id,
                user_account,
                rate: _rate,
            } => {
                PaymentStream::create(conn, provider_id.to_string(), user_account.to_string())
                    .await?;
            }
            pallet_payment_streams::Event::FixedRatePaymentStreamUpdated { .. } => {
                // TODO: Currently we are not treating the info of fixed rate update
            }
            pallet_payment_streams::Event::FixedRatePaymentStreamDeleted { .. } => {}
            pallet_payment_streams::Event::PaymentStreamCharged {
                user_account,
                provider_id,
                amount,
                last_tick_charged,
                charged_at_tick,
            } => {
                // We want to handle this and update the payment stream total amount
                let ps =
                    PaymentStream::get(conn, user_account.to_string(), provider_id.to_string())
                        .await?;
                let new_total_amount = ps.total_amount_paid + amount;
                let last_tick_charged: i64 = (*last_tick_charged).into();
                let charged_at_tick: i64 = (*charged_at_tick).into();
                PaymentStream::update_total_amount(
                    conn,
                    ps.id,
                    new_total_amount,
                    last_tick_charged,
                    charged_at_tick,
                )
                .await?;
            }
            pallet_payment_streams::Event::LastChargeableInfoUpdated { .. } => {}
            pallet_payment_streams::Event::UserWithoutFunds { .. } => {}
            pallet_payment_streams::Event::UserPaidDebts { .. } => {}
            pallet_payment_streams::Event::UserSolvent { .. } => {}
            pallet_payment_streams::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }

    async fn index_proofs_dealer_event<'a, 'b: 'a>(
        &'b self,
        _conn: &mut DbConnection<'a>,
        event: &pallet_proofs_dealer::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_proofs_dealer::Event::MutationsApplied { .. } => {}
            pallet_proofs_dealer::Event::NewChallenge { .. } => {}
            pallet_proofs_dealer::Event::ProofAccepted { .. } => {}
            pallet_proofs_dealer::Event::NewChallengeSeed { .. } => {}
            pallet_proofs_dealer::Event::NewCheckpointChallenge { .. } => {}
            pallet_proofs_dealer::Event::SlashableProvider { .. } => {}
            pallet_proofs_dealer::Event::NoRecordOfLastSubmittedProof { .. } => {}
            pallet_proofs_dealer::Event::NewChallengeCycleInitialised { .. } => {}
            pallet_proofs_dealer::Event::ChallengesTickerSet { .. } => {}
            pallet_proofs_dealer::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }

    async fn index_providers_event<'a, 'b: 'a>(
        &'b self,
        conn: &mut DbConnection<'a>,
        event: &pallet_storage_providers::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_storage_providers::Event::BspRequestSignUpSuccess { .. } => {}
            pallet_storage_providers::Event::BspSignUpSuccess {
                who,
                bsp_id,
                multiaddresses,
                capacity,
            } => {
                let mut sql_multiaddresses = Vec::new();
                for multiaddress in multiaddresses {
                    let multiaddress_str =
                        String::from_utf8(multiaddress.to_vec()).expect("Invalid multiaddress");
                    sql_multiaddresses.push(MultiAddress::create(conn, multiaddress_str).await?);
                }

                Bsp::create(
                    conn,
                    who.to_string(),
                    capacity.into(),
                    sql_multiaddresses,
                    bsp_id.to_string(),
                )
                .await?;
            }
            pallet_storage_providers::Event::BspSignOffSuccess {
                who,
                bsp_id: _bsp_id,
            } => {
                Bsp::delete(conn, who.to_string()).await?;
            }
            pallet_storage_providers::Event::CapacityChanged {
                who,
                new_capacity,
                provider_id,
                old_capacity: _old_capacity,
                next_block_when_change_allowed: _next_block_when_change_allowed,
            } => match provider_id {
                StorageProviderId::BackupStorageProvider(_) => {
                    Bsp::update_capacity(conn, who.to_string(), new_capacity.into()).await?;
                }
                StorageProviderId::MainStorageProvider(_) => {
                    Bsp::update_capacity(conn, who.to_string(), new_capacity.into()).await?;
                }
            },
            pallet_storage_providers::Event::SignUpRequestCanceled { .. } => {}
            pallet_storage_providers::Event::MspRequestSignUpSuccess { .. } => {}
            pallet_storage_providers::Event::MspSignUpSuccess {
                who,
                msp_id,
                multiaddresses,
                capacity,
                value_prop,
            } => {
                let mut sql_multiaddresses = Vec::new();
                for multiaddress in multiaddresses {
                    let multiaddress_str =
                        String::from_utf8(multiaddress.to_vec()).expect("Invalid multiaddress");
                    sql_multiaddresses.push(MultiAddress::create(conn, multiaddress_str).await?);
                }

                // TODO: update value prop after properly defined in runtime
                let value_prop = format!("{value_prop:?}");

                Msp::create(
                    conn,
                    who.to_string(),
                    capacity.into(),
                    value_prop,
                    sql_multiaddresses,
                    msp_id.to_string(),
                )
                .await?;
            }
            pallet_storage_providers::Event::MspSignOffSuccess {
                who,
                msp_id: _msp_id,
            } => {
                Msp::delete(conn, who.to_string()).await?;
            }
            pallet_storage_providers::Event::Slashed { .. } => {}
            pallet_storage_providers::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }

    async fn index_randomness_event<'a, 'b: 'a>(
        &'b self,
        _conn: &mut DbConnection<'a>,
        event: &pallet_randomness::Event<storage_hub_runtime::Runtime>,
    ) -> Result<(), diesel::result::Error> {
        match event {
            pallet_randomness::Event::NewOneEpochAgoRandomnessAvailable { .. } => {}
            pallet_randomness::Event::__Ignore(_, _) => {}
        }
        Ok(())
    }
}

// Define the EventLoop for IndexerService
pub struct IndexerServiceEventLoop {
    receiver: sc_utils::mpsc::TracingUnboundedReceiver<IndexerServiceCommand>,
    actor: IndexerService,
}

enum MergedEventLoopMessage<Block>
where
    Block: sp_runtime::traits::Block,
{
    Command(IndexerServiceCommand),
    FinalityNotification(sc_client_api::FinalityNotification<Block>),
}

// Implement ActorEventLoop for IndexerServiceEventLoop
impl ActorEventLoop<IndexerService> for IndexerServiceEventLoop {
    fn new(
        actor: IndexerService,
        receiver: sc_utils::mpsc::TracingUnboundedReceiver<IndexerServiceCommand>,
    ) -> Self {
        Self { actor, receiver }
    }

    async fn run(mut self) {
        info!(target: LOG_TARGET, "IndexerService starting up!");

        let finality_notification_stream = self.actor.client.finality_notification_stream();

        let mut merged_stream = stream::select(
            self.receiver.map(MergedEventLoopMessage::Command),
            finality_notification_stream.map(MergedEventLoopMessage::FinalityNotification),
        );

        while let Some(message) = merged_stream.next().await {
            match message {
                MergedEventLoopMessage::Command(command) => {
                    self.actor.handle_message(command).await;
                }
                MergedEventLoopMessage::FinalityNotification(notification) => {
                    self.actor
                        .handle_finality_notification(notification)
                        .await
                        .unwrap_or_else(|e| {
                            error!(target: LOG_TARGET, "Failed to handle finality notification: {}", e);
                        });
                }
            }
        }

        info!(target: LOG_TARGET, "IndexerService shutting down.");
    }
}

#[derive(Error, Debug)]
pub enum IndexBlockError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] diesel::result::Error),
    #[error("Failed to retrieve or decode events: {0}")]
    EventsRetrievalError(#[from] EventsRetrievalError),
}

#[derive(Error, Debug)]
pub enum HandleFinalityNotificationError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] diesel::result::Error),
    #[error("Block hash not found")]
    BlockHashNotFound,
    #[error("Index block error: {0}")]
    IndexBlockError(#[from] IndexBlockError),
    #[error("Client error: {0}")]
    ClientError(#[from] sp_blockchain::Error),
    #[error("Pool run error: {0}")]
    PoolRunError(#[from] diesel_async::pooled_connection::bb8::RunError),
}