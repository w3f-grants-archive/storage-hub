//! Various types to use in the Storage Providers pallet.

use super::*;
use codec::{Decode, Encode, MaxEncodedLen};
use frame_support::pallet_prelude::*;
use frame_support::traits::fungible::Inspect;
use frame_system::pallet_prelude::BlockNumberFor;
use scale_info::TypeInfo;
use sp_runtime::BoundedVec;

pub type Multiaddresses<T> = BoundedVec<MultiAddress<T>, MaxMultiAddressAmount<T>>;

pub type ValuePropId<T> = HashId<T>;

#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct ValuePropositionWithId<T: Config> {
    pub id: ValuePropId<T>,
    pub value_prop: ValueProposition<T>,
}

#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct ValueProposition<T: Config> {
    pub price_per_unit_of_data_per_block: BalanceOf<T>,
    pub commitment: Commitment<T>,
    /// Maximum [`StorageDataUnit`]s that can be stored in a bucket.
    pub bucket_data_limit: StorageDataUnit<T>,
    /// Newly created buckets can only specify available value propositions.
    /// Any existing bucket with an unavailable value proposition are not affected.
    pub available: bool,
}

impl<T: Config> ValueProposition<T> {
    pub fn new(
        price_per_unit_of_data_per_block: BalanceOf<T>,
        commitment: Commitment<T>,
        bucket_data_limit: StorageDataUnit<T>,
    ) -> Self {
        Self {
            price_per_unit_of_data_per_block,
            commitment,
            bucket_data_limit,
            available: true,
        }
    }

    /// Produce the ID of the ValueProposition not including the `available` field.
    pub fn derive_id(&self) -> HashId<T> {
        let mut concat = self.price_per_unit_of_data_per_block.encode();
        concat.extend_from_slice(&self.commitment.encode());
        concat.extend_from_slice(&self.bucket_data_limit.encode());
        <<T as frame_system::Config>::Hashing as sp_runtime::traits::Hash>::hash(&concat)
    }
}

pub type Commitment<T> = BoundedVec<u8, <T as crate::Config>::MaxCommitmentSize>;

/// Structure that represents a Main Storage Provider. It holds the buckets that the MSP has, the total data that the MSP is able to store,
/// the amount of data that it is storing, and its libp2p multiaddresses.
#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct MainStorageProvider<T: Config> {
    pub buckets: Buckets<T>,
    pub capacity: StorageDataUnit<T>,
    pub capacity_used: StorageDataUnit<T>,
    pub multiaddresses: Multiaddresses<T>,
    pub last_capacity_change: BlockNumberFor<T>,
    pub owner_account: T::AccountId,
    pub payment_account: T::AccountId,
    pub sign_up_block: BlockNumberFor<T>,
}

/// Structure that represents a Backup Storage Provider. It holds the total data that the BSP is able to store, the amount of data that it is storing,
/// its libp2p multiaddresses, and the root of the Merkle Patricia Trie that it stores.
#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct BackupStorageProvider<T: Config> {
    pub capacity: StorageDataUnit<T>,
    pub capacity_used: StorageDataUnit<T>,
    pub multiaddresses: Multiaddresses<T>,
    pub root: MerklePatriciaRoot<T>,
    pub last_capacity_change: BlockNumberFor<T>,
    pub owner_account: T::AccountId,
    pub payment_account: T::AccountId,
    pub reputation_weight: ReputationWeightType<T>,
    pub sign_up_block: BlockNumberFor<T>,
}

/// Structure that represents a Bucket. It holds the root of the Merkle Patricia Trie, the User ID that owns the bucket,
/// and the MainStorageProviderId that the bucket belongs to.
#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct Bucket<T: Config> {
    pub root: MerklePatriciaRoot<T>,
    pub user_id: T::AccountId,
    pub msp_id: MainStorageProviderId<T>,
    pub private: bool,
    pub read_access_group_id: Option<T::ReadAccessGroupId>,
    pub size: StorageDataUnit<T>,
    pub value_prop_id: HashId<T>,
}

#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct SignUpRequest<T: Config> {
    pub sp_sign_up_request: SignUpRequestSpParams<T>,
    pub at: BlockNumberFor<T>,
}

/// Enum that represents a Storage Provider sign up request parameters. It holds either a BackupStorageProvider or a MainStorageProvider,
/// allowing to operate generically with both types.
#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub enum SignUpRequestSpParams<T: Config> {
    BackupStorageProvider(BackupStorageProvider<T>),
    MainStorageProvider(MainStorageProviderSignUpRequest<T>),
}

#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub struct MainStorageProviderSignUpRequest<T: Config> {
    pub msp_info: MainStorageProvider<T>,
    pub value_prop: ValueProposition<T>,
}

/// Enum that represents a Storage Provider ID. It holds either a BackupStorageProviderId or a MainStorageProviderId,
/// allowing to operate generically with both types.
#[derive(Encode, Decode, MaxEncodedLen, TypeInfo, RuntimeDebugNoBound, PartialEq, Eq, Clone)]
#[scale_info(skip_type_params(T))]
pub enum StorageProviderId<T: Config> {
    BackupStorageProvider(BackupStorageProviderId<T>),
    MainStorageProvider(MainStorageProviderId<T>),
}

// Type aliases:

/// BalanceOf is the balance type of the runtime.
pub type BalanceOf<T> =
    <<T as Config>::NativeBalance as Inspect<<T as frame_system::Config>::AccountId>>::Balance;

pub type ProviderId<T> = HashId<T>;
/// BackupStorageProviderId is the type that represents an ID of a Backup Storage Provider, uniquely linked with an AccountId
pub type BackupStorageProviderId<T> = ProviderId<T>;
/// MainStorageProviderId is the type that represents an ID of a Main Storage Provider, uniquely linked with an AccountId
pub type MainStorageProviderId<T> = ProviderId<T>;
/// BucketId is the type that identifies the different buckets that a Main Storage Provider can have.
pub type BucketId<T> = HashId<T>;

/// MaxMultiAddressSize is the maximum size of the libp2p multiaddress of a Storage Provider in bytes.
pub type MaxMultiAddressSize<T> = <T as crate::Config>::MaxMultiAddressSize;
/// MaxMultiAddressAmount is the maximum amount of MultiAddresses that a Storage Provider can have.
pub type MaxMultiAddressAmount<T> = <T as crate::Config>::MaxMultiAddressAmount;
/// MultiAddress is a byte array that represents the libp2p multiaddress of a Storage Provider.
/// Its maximum size is defined in the runtime configuration, as MaxMultiAddressSize.
pub type MultiAddress<T> = BoundedVec<u8, MaxMultiAddressSize<T>>;

/// MerklePatriciaRoot is the type of the root of a Merkle Patricia Trie, either the root of a BSP or a bucket from an MSP.
pub type MerklePatriciaRoot<T> = <T as crate::Config>::MerklePatriciaRoot;

/// HashId is the type that uniquely identifies either a Storage Provider (MSP or BSP) or a Bucket.
pub type HashId<T> = <T as frame_system::Config>::Hash;

/// StorageData is the type of the unit in which we measure data size. We define its required traits in the
/// pallet configuration so the runtime can use any type that implements them.
pub type StorageDataUnit<T> = <T as crate::Config>::StorageDataUnit;

/// Protocols is a vector of the protocols that (the runtime is aware of and) the Main Storage Provider supports.
/// Its maximum size is defined in the runtime configuration, as MaxProtocols.
pub type MaxProtocols<T> = <T as crate::Config>::MaxProtocols;
pub type Protocols<T> = BoundedVec<u8, MaxProtocols<T>>; // todo!("Define a type for protocols")

/// MaxBuckets is the maximum amount of buckets that a Main Storage Provider can have.
pub type MaxBuckets<T> = <T as crate::Config>::MaxBuckets;
/// Buckets is a vector of the buckets that a Main Storage Provider has.
pub type Buckets<T> = BoundedVec<Bucket<T>, MaxBuckets<T>>;

/// Type alias for the `ReputationWeightType` type used in the Storage Providers pallet.
pub type ReputationWeightType<T> = <T as crate::Config>::ReputationWeightType;

/// Type alias for the `StartingReputationWeight` type used in the Storage Providers pallet.
pub type StartingReputationWeight<T> = <T as crate::Config>::StartingReputationWeight;
