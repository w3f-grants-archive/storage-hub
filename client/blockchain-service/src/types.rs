use std::{
    cmp::{min, Ordering},
    future::Future,
    pin::Pin,
    time::Duration,
};

use codec::{Decode, Encode};
use frame_support::dispatch::DispatchInfo;
use log::warn;
use sc_client_api::BlockImportNotification;
use shc_common::types::{
    BlockNumber, HasherOutT, ProofsDealerProviderId, RandomnessOutput,
    RejectedStorageRequestReason, StorageHubEventsVec, StorageProofsMerkleTrieLayout,
    TrieRemoveMutation,
};
use sp_core::H256;
use sp_runtime::{traits::Header, AccountId32, DispatchError, SaturatedConversion};

use crate::handler::LOG_TARGET;

/// A struct that holds the information to submit a storage proof.
///
/// This struct is used as an item in the `pending_submit_proof_requests` queue.
#[derive(Debug, Clone, Encode, Decode)]
pub struct SubmitProofRequest {
    pub provider_id: ProofsDealerProviderId,
    pub tick: BlockNumber,
    pub seed: RandomnessOutput,
    pub forest_challenges: Vec<H256>,
    pub checkpoint_challenges: Vec<(H256, Option<TrieRemoveMutation>)>,
}

impl SubmitProofRequest {
    pub fn new(
        provider_id: ProofsDealerProviderId,
        tick: BlockNumber,
        seed: RandomnessOutput,
        forest_challenges: Vec<H256>,
        checkpoint_challenges: Vec<(H256, Option<TrieRemoveMutation>)>,
    ) -> Self {
        Self {
            provider_id,
            tick,
            seed,
            forest_challenges,
            checkpoint_challenges,
        }
    }
}

impl Ord for SubmitProofRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tick.cmp(&other.tick)
    }
}

impl PartialOrd for SubmitProofRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Two `SubmitProofRequest`s are considered equal if they have the same `tick` and `provider_id`.
// This helps to identify and remove duplicate requests from the queue.
impl PartialEq for SubmitProofRequest {
    fn eq(&self, other: &Self) -> bool {
        self.tick == other.tick && self.provider_id == other.provider_id
    }
}

impl Eq for SubmitProofRequest {}

#[derive(Debug, Clone, Encode, Decode)]
pub struct ConfirmStoringRequest {
    pub file_key: H256,
    pub try_count: u32,
}

impl ConfirmStoringRequest {
    pub fn new(file_key: H256) -> Self {
        Self {
            file_key,
            try_count: 0,
        }
    }

    pub fn increment_try_count(&mut self) {
        self.try_count += 1;
    }
}

#[derive(Debug, Clone, Encode, Decode)]
pub enum MspRespondStorageRequest {
    Accept,
    Reject(RejectedStorageRequestReason),
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct RespondStorageRequest {
    pub file_key: H256,
    pub response: MspRespondStorageRequest,
    pub try_count: u32,
}

impl RespondStorageRequest {
    pub fn new(file_key: H256, response: MspRespondStorageRequest) -> Self {
        Self {
            file_key,
            response,
            try_count: 0,
        }
    }

    pub fn increment_try_count(&mut self) {
        self.try_count += 1;
    }
}

/// A struct that holds the information to stop storing all files from an insolvent user.
/// (Which is only the user's account ID).
///
/// This struct is used as an item in the `pending_stop_storing_for_insolvent_user_requests` queue.
#[derive(Debug, Clone, Encode, Decode)]
pub struct StopStoringForInsolventUserRequest {
    pub user: AccountId32,
}

impl StopStoringForInsolventUserRequest {
    pub fn new(user: AccountId32) -> Self {
        Self { user }
    }
}

/// Extrinsic struct.
///
/// This struct represents an extrinsic in the blockchain.
#[derive(Debug, Clone)]
pub struct Extrinsic {
    /// Extrinsic hash.
    pub hash: H256,
    /// Block hash.
    pub block_hash: H256,
    /// Events vector.
    pub events: StorageHubEventsVec,
}

/// ExtrinsicResult enum.
///
/// This enum represents the result of an extrinsic execution. It can be either a success or a failure.
pub enum ExtrinsicResult {
    /// Success variant.
    ///
    /// This variant represents a successful extrinsic execution.
    Success {
        /// Dispatch info.
        dispatch_info: DispatchInfo,
    },
    /// Failure variant.
    ///
    /// This variant represents a failed extrinsic execution.
    Failure {
        /// Dispatch error.
        dispatch_error: DispatchError,
        /// Dispatch info.
        dispatch_info: DispatchInfo,
    },
}

/// Type alias for the extrinsic hash.
pub type ExtrinsicHash = H256;

/// Type alias for the tip.
pub type Tip = pallet_transaction_payment::ChargeTransactionPayment<storage_hub_runtime::Runtime>;

/// A struct which defines a submit extrinsic retry strategy. This defines a simple strategy when
/// sending and extrinsic. It will retry a maximum number of times ([Self::max_retries]).
/// If the extrinsic is not included in a block within a certain time frame [`Self::timeout`] it is
/// considered a failure.
/// The tip will increase with each retry, up to a maximum tip of [`Self::max_tip`].
/// The tip series (with the exception of the first try which is 0) is a geometric progression with
/// a multiplier of [`Self::base_multiplier`].
/// The final tip for each retry is calculated as:
/// [`Self::max_tip`] * (([`Self::base_multiplier`] ^ (retry_count / [`Self::max_retries`]) - 1) /
/// ([`Self::base_multiplier`] - 1)).
/// An optional check function can be provided to determine if the extrinsic should be retried,
/// aborting early if the function returns false.
pub struct RetryStrategy {
    /// Maximum number of retries after which the extrinsic submission will be considered failed.
    pub max_retries: u32,
    /// Maximum time to wait for a response before assuming the extrinsic submission has failed.
    pub timeout: Duration,
    /// Maximum tip to be paid for the extrinsic submission. The progression follows an exponential
    /// backoff strategy.
    pub max_tip: f64,
    /// Base multiplier for the tip calculation. This is the base of the geometric progression.
    /// A higher value will make tips grow faster.
    pub base_multiplier: f64,
    /// An optional check function to determine if the extrinsic should be retried.
    /// If this is provided, the function will be called before each retry to determine if the
    /// extrinsic should be retried or the submission should be considered failed. If this is not
    /// provided, the extrinsic will be retried until [`Self::max_retries`] is reached.
    pub should_retry: Option<Box<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send>>,
}

impl RetryStrategy {
    /// Creates a new `RetryStrategy` instance.
    pub fn new(max_retries: u32, timeout: Duration, max_tip: f64, base_multiplier: f64) -> Self {
        Self {
            max_retries,
            timeout,
            max_tip,
            base_multiplier,
            should_retry: None,
        }
    }

    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_tip(mut self, max_tip: f64) -> Self {
        self.max_tip = max_tip;
        self
    }

    pub fn with_base_multiplier(mut self, base_multiplier: f64) -> Self {
        self.base_multiplier = base_multiplier;
        self
    }

    pub fn with_should_retry(
        mut self,
        should_retry: Option<Box<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send>>,
    ) -> Self {
        self.should_retry = should_retry;
        self
    }

    /// Computes the tip for the given retry count.
    /// The formula for the tip is:
    /// [`Self::max_tip`] * (([`Self::base_multiplier`] ^ (retry_count / [`Self::max_retries`]) - 1) /
    /// ([`Self::base_multiplier`] - 1)).
    pub fn compute_tip(&self, retry_count: u32) -> f64 {
        // Ensure the retry_count is within the bounds of max_retries
        let retry_count = min(retry_count, self.max_retries);

        // Calculate the geometric progression factor for this retry_count
        let factor = (self
            .base_multiplier
            .powf(retry_count as f64 / self.max_retries as f64)
            - 1.0)
            / (self.base_multiplier - 1.0);

        // Final tip formula for each retry, scaled to max_tip
        self.max_tip * factor
    }
}

impl Default for RetryStrategy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            timeout: Duration::from_secs(30),
            max_tip: 0.0,
            base_multiplier: 2.0,
            should_retry: None,
        }
    }
}

/// Minimum block information needed to register what is the current best block
/// and detect reorgs.
#[derive(Debug, Clone, Encode, Decode, Default, Copy)]
pub struct BestBlockInfo {
    pub number: BlockNumber,
    pub hash: H256,
}

impl<Block> From<&BlockImportNotification<Block>> for BestBlockInfo
where
    Block: cumulus_primitives_core::BlockT<Hash = H256>,
{
    fn from(notification: &BlockImportNotification<Block>) -> Self {
        Self {
            number: (*notification.header.number()).saturated_into(),
            hash: notification.hash,
        }
    }
}

impl<Block> From<BlockImportNotification<Block>> for BestBlockInfo
where
    Block: cumulus_primitives_core::BlockT<Hash = H256>,
{
    fn from(notification: BlockImportNotification<Block>) -> Self {
        Self {
            number: (*notification.header.number()).saturated_into(),
            hash: notification.hash,
        }
    }
}

/// When a new block is imported, the block is checked to see if it is one of the members
/// of this enum.
pub enum NewBlockNotificationKind {
    /// The block is a new best block, built on top of the previous best block.
    NewBestBlock(BestBlockInfo),
    /// The block belongs to a fork that is not currently the best fork.
    NewNonBestBlock(BestBlockInfo),
    /// This fork causes a reorg, i.e. it is the new best block, but the previous best block
    /// is not the parent of this one.
    ///
    /// The old best block (from the now non-best fork) is provided, as well as the new best block.
    Reorg {
        old_best_block: BestBlockInfo,
        new_best_block: BestBlockInfo,
    },
}

/// The information needed to register a Forest Storage snapshot.
#[derive(Debug, Clone, Encode, Decode, PartialEq, Eq)]
pub struct ForestStorageSnapshotInfo {
    /// The block number at which the Forest Storage snapshot was taken.
    ///
    /// i.e. the block number at which the Forest Storage changed from this snapshot
    /// version due to adding or removing files.
    pub block_number: BlockNumber,
    /// The Forest Storage snapshot hash.
    ///
    /// This is to uniquely identify the Forest Storage snapshot, as there could be
    /// snapshots at the same block number, but in different forks.
    pub block_hash: H256,
    /// The Forest Storage root when the snapshot was taken.
    ///
    /// This is used to identify the Forest Storage snapshot and retrieve it.
    pub forest_root: HasherOutT<StorageProofsMerkleTrieLayout>,
}

impl PartialOrd for ForestStorageSnapshotInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Implements the `Ord` trait for `ForestStorageSnapshotInfo`.
///
/// This allows for a BTreeSet to be used to store Forest Storage snapshots.
impl Ord for ForestStorageSnapshotInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Block number ordering is the first criteria.
        match self.block_number.cmp(&other.block_number) {
            std::cmp::Ordering::Less => std::cmp::Ordering::Less,
            std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
            std::cmp::Ordering::Equal => {
                // If the block numbers are equal, compare the block hashes.
                match self.block_hash.cmp(&other.block_hash) {
                    std::cmp::Ordering::Less => std::cmp::Ordering::Less,
                    std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
                    std::cmp::Ordering::Equal => {
                        // If the block hashes and block numbers are equal, the forest roots should be
                        // the same, because there can only be one snapshot at a given block number,
                        // for a given fork.
                        if self.forest_root != other.forest_root {
                            warn!(target: LOG_TARGET, "CRITICAL❗️❗️ This is a bug! Forest storage snapshot forest roots are not equal, for the same block number and hash. This should not happen. This is a bug. Please report it to the StorageHub team.");
                        }

                        std::cmp::Ordering::Equal
                    }
                }
            }
        }
    }
}
