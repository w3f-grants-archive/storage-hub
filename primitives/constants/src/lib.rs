#![cfg_attr(not(feature = "std"), no_std)]

use sp_core::Hasher;
use sp_runtime::traits::BlakeTwo256;

/// The size of the hash output in bytes.
pub const H_LENGTH: usize = BlakeTwo256::LENGTH;

/// The file chunk size in bytes. This is the size of the leaf nodes in the Merkle
/// Patricia Trie that is constructed for each file.
/// Each chunk is 1 kB.
pub const FILE_CHUNK_SIZE: u64 = 2u64.pow(10);

/// The number of challenges for a file, depending on the size of the file.
/// For every 512 kB, there is a challenge.
#[cfg(feature = "runtime-benchmarks")]
pub const FILE_SIZE_TO_CHALLENGES: u64 = 2u64.pow(10);
#[cfg(not(feature = "runtime-benchmarks"))]
pub const FILE_SIZE_TO_CHALLENGES: u64 = 2u64.pow(19);

/// The amount of units that fit in a gigaunit.
pub const GIGAUNIT: u32 = 2u32.pow(30);
