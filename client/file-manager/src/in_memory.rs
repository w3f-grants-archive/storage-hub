use sp_trie::{recorder::Recorder, MemoryDB, Trie, TrieDBBuilder, TrieLayout, TrieMut};
use std::collections::{HashMap, HashSet};
use trie_db::TrieDBMutBuilder;

use shc_common::types::{
    Chunk, ChunkId, ChunkWithId, FileKeyProof, FileMetadata, FileProof, HashT, HasherOutT, H_LENGTH,
};

use crate::traits::{
    FileDataTrie, FileStorage, FileStorageError, FileStorageWriteError, FileStorageWriteOutcome,
};

use codec::{Decode, Encode};

pub struct InMemoryFileDataTrie<T: TrieLayout + 'static> {
    root: HasherOutT<T>,
    memdb: MemoryDB<T::Hash>,
}

impl<T: TrieLayout + 'static> InMemoryFileDataTrie<T> {
    fn new() -> Self {
        let (memdb, root) = MemoryDB::<HashT<T>>::default_with_root();

        Self { root, memdb }
    }
}

impl<T: TrieLayout> FileDataTrie<T> for InMemoryFileDataTrie<T> {
    fn get_root(&self) -> &HasherOutT<T> {
        &self.root
    }

    fn stored_chunks_count(&self) -> Result<u64, FileStorageError> {
        let trie = TrieDBBuilder::<T>::new(&self.memdb, &self.root).build();
        let trie_iter = trie
            .iter()
            .map_err(|_| FileStorageError::FailedToConstructTrieIter)?;
        let stored_chunks = trie_iter.count() as u64;

        Ok(stored_chunks)
    }

    fn generate_proof(&self, chunk_ids: &Vec<ChunkId>) -> Result<FileProof, FileStorageError> {
        let recorder: Recorder<T::Hash> = Recorder::default();

        // A `TrieRecorder` is needed to create a proof of the "visited" leafs, by the end of this process.
        let mut trie_recorder = recorder.as_trie_recorder(self.root);

        let trie = TrieDBBuilder::<T>::new(&self.memdb, &self.root)
            .with_recorder(&mut trie_recorder)
            .build();

        // Read all the chunks to prove from the trie.
        let mut chunks = Vec::new();
        for chunk_id in chunk_ids {
            // Get the encoded chunk from the trie.
            let encoded_chunk: Vec<u8> = trie
                .get(&chunk_id.as_trie_key())
                .map_err(|_| FileStorageError::FailedToGetFileChunk)?
                .ok_or(FileStorageError::FileChunkDoesNotExist)?;

            // Decode it to its chunk ID and data.
            let decoded_chunk = ChunkWithId::decode(&mut encoded_chunk.as_slice())
                .map_err(|_| FileStorageError::FailedToParseChunkWithId)?;

            chunks.push((decoded_chunk.chunk_id, decoded_chunk.data));
        }

        // Drop the `trie_recorder` to release the `recorder`
        drop(trie_recorder);

        // Generate proof
        let proof = recorder
            .drain_storage_proof()
            .to_compact_proof::<T::Hash>(self.root)
            .map_err(|_| FileStorageError::FailedToGenerateCompactProof)?;

        Ok(FileProof {
            proof: proof.into(),
            fingerprint: self.get_root().as_ref().into(),
        })
    }

    fn get_chunk(&self, chunk_id: &ChunkId) -> Result<Chunk, FileStorageError> {
        let trie = TrieDBBuilder::<T>::new(&self.memdb, &self.root).build();

        // Get the encoded chunk from the trie.
        let encoded_chunk = trie
            .get(&chunk_id.as_trie_key())
            .map_err(|_| FileStorageError::FailedToGetFileChunk)?
            .ok_or(FileStorageError::FileChunkDoesNotExist)?;

        // Decode it to its chunk ID and data.
        let decoded_chunk = ChunkWithId::decode(&mut encoded_chunk.as_slice())
            .map_err(|_| FileStorageError::FailedToParseChunkWithId)?;

        // Return the chunk data.
        Ok(decoded_chunk.data)
    }

    fn write_chunk(
        &mut self,
        chunk_id: &ChunkId,
        data: &Chunk,
    ) -> Result<(), FileStorageWriteError> {
        let mut trie = if self.memdb.keys().is_empty() {
            // If the database is empty, create a new trie.
            TrieDBMutBuilder::<T>::new(&mut self.memdb, &mut self.root).build()
        } else {
            // If the database is not empty, build the trie from an existing root and memdb.
            TrieDBMutBuilder::<T>::from_existing(&mut self.memdb, &mut self.root).build()
        };

        // Check that we don't have a chunk already stored.
        if trie
            .contains(&chunk_id.as_trie_key())
            .map_err(|_| FileStorageWriteError::FailedToGetFileChunk)?
        {
            return Err(FileStorageWriteError::FileChunkAlreadyExists);
        }

        // Insert the encoded chunk with its ID into the file trie.
        let decoded_chunk = ChunkWithId {
            chunk_id: *chunk_id,
            data: data.clone(),
        };
        let encoded_chunk = decoded_chunk.encode();
        trie.insert(&chunk_id.as_trie_key(), &encoded_chunk)
            .map_err(|_| FileStorageWriteError::FailedToInsertFileChunk)?;

        // dropping the trie automatically commits changes to the underlying db
        drop(trie);

        Ok(())
    }

    fn delete(&mut self) -> Result<(), FileStorageWriteError> {
        let (memdb, root) = MemoryDB::<HashT<T>>::default_with_root();
        self.root = root;
        self.memdb = memdb;

        Ok(())
    }
}

pub struct InMemoryFileStorage<T: TrieLayout + 'static>
where
    HasherOutT<T>: TryFrom<[u8; H_LENGTH]>,
{
    pub metadata: HashMap<HasherOutT<T>, FileMetadata>,
    pub file_data: HashMap<HasherOutT<T>, InMemoryFileDataTrie<T>>,
    pub bucket_prefix_map: HashSet<[u8; 64]>,
}

impl<T: TrieLayout> InMemoryFileStorage<T>
where
    HasherOutT<T>: TryFrom<[u8; H_LENGTH]>,
{
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            file_data: HashMap::new(),
            bucket_prefix_map: HashSet::new(),
        }
    }
}

impl<T: TrieLayout + 'static> FileStorage<T> for InMemoryFileStorage<T>
where
    HasherOutT<T>: TryFrom<[u8; H_LENGTH]>,
{
    type FileDataTrie = InMemoryFileDataTrie<T>;

    fn new_file_data_trie(&self) -> Self::FileDataTrie {
        InMemoryFileDataTrie::new()
    }

    fn generate_proof(
        &self,
        file_key: &HasherOutT<T>,
        chunk_id: &Vec<ChunkId>,
    ) -> Result<FileKeyProof, FileStorageError> {
        let metadata = self
            .metadata
            .get(file_key)
            .ok_or(FileStorageError::FileDoesNotExist)?;

        let file_data = self.file_data.get(file_key).expect(
            format!(
                "Invariant broken! Metadata for file key {:?} found but no associated trie",
                file_key
            )
            .as_str(),
        );

        let stored_chunks = file_data.stored_chunks_count()?;
        if metadata.chunks_count() != stored_chunks {
            return Err(FileStorageError::IncompleteFile);
        }

        if metadata.fingerprint
            != file_data
                .get_root()
                .as_ref()
                .try_into()
                .expect("Hasher output mismatch!")
        {
            return Err(FileStorageError::FingerprintAndStoredFileMismatch);
        }

        Ok(file_data
            .generate_proof(chunk_id)?
            .to_file_key_proof(metadata.clone()))
    }

    fn delete_file(&mut self, key: &HasherOutT<T>) -> Result<(), FileStorageError> {
        self.metadata.remove(key);
        self.file_data.remove(key);

        Ok(())
    }

    fn get_metadata(
        &self,
        file_key: &HasherOutT<T>,
    ) -> Result<Option<FileMetadata>, FileStorageError> {
        Ok(self.metadata.get(file_key).cloned())
    }

    fn insert_file(
        &mut self,
        key: HasherOutT<T>,
        metadata: FileMetadata,
    ) -> Result<(), FileStorageError> {
        if self.metadata.contains_key(&key) {
            return Err(FileStorageError::FileAlreadyExists);
        }
        self.metadata.insert(key, metadata.clone());

        let empty_file_trie = self.new_file_data_trie();
        let previous = self.file_data.insert(key, empty_file_trie);
        if previous.is_some() {
            panic!("Key already associated with File Data, but not with File Metadata. Possible inconsistency between them.");
        }

        let full_key = [metadata.bucket_id.as_slice(), key.as_ref()].concat();
        self.bucket_prefix_map.insert(full_key.try_into().unwrap());

        Ok(())
    }

    fn insert_file_with_data(
        &mut self,
        key: HasherOutT<T>,
        metadata: FileMetadata,
        file_data: Self::FileDataTrie,
    ) -> Result<(), FileStorageError> {
        if self.metadata.contains_key(&key) {
            return Err(FileStorageError::FileAlreadyExists);
        }
        self.metadata.insert(key, metadata.clone());

        let previous = self.file_data.insert(key, file_data);
        if previous.is_some() {
            panic!("Key already associated with File Data, but not with File Metadata. Possible inconsistency between them.");
        }

        let full_key = [metadata.bucket_id.as_slice(), key.as_ref()].concat();
        self.bucket_prefix_map.insert(full_key.try_into().unwrap());

        Ok(())
    }

    fn stored_chunks_count(&self, key: &HasherOutT<T>) -> Result<u64, FileStorageError> {
        let file_data = self.file_data.get(key);
        let file_data = file_data.ok_or(FileStorageError::FileDoesNotExist)?;

        file_data.stored_chunks_count()
    }

    fn get_chunk(
        &self,
        file_key: &HasherOutT<T>,
        chunk_id: &ChunkId,
    ) -> Result<Chunk, FileStorageError> {
        let file_data = self.file_data.get(file_key);
        let file_data = file_data.ok_or(FileStorageError::FileDoesNotExist)?;

        file_data.get_chunk(chunk_id)
    }

    fn write_chunk(
        &mut self,
        file_key: &HasherOutT<T>,
        chunk_id: &ChunkId,
        data: &Chunk,
    ) -> Result<FileStorageWriteOutcome, FileStorageWriteError> {
        let file_data = self
            .file_data
            .get_mut(file_key)
            .ok_or(FileStorageWriteError::FileDoesNotExist)?;

        file_data.write_chunk(chunk_id, data)?;

        let metadata = self.metadata.get(file_key).expect(
            format!("Key {:?} already associated with File Trie, but no File Metadata. Possible inconsistency between them.",
            file_key
        )
            .as_str(),
        );

        // Check if we have all the chunks for the file.
        let stored_chunks = file_data
            .stored_chunks_count()
            .map_err(|_| FileStorageWriteError::FailedToConstructTrieIter)?;
        if metadata.chunks_count() != stored_chunks {
            return Ok(FileStorageWriteOutcome::FileIncomplete);
        }

        // If we have all the chunks, check if the file metadata fingerprint and the file trie
        // root matches.
        if metadata.fingerprint != file_data.get_root().as_ref().into() {
            return Err(FileStorageWriteError::FingerprintAndStoredFileMismatch);
        }

        Ok(FileStorageWriteOutcome::FileComplete)
    }

    fn delete_files_with_prefix(&mut self, prefix: &[u8; 32]) -> Result<(), FileStorageError>
    where
        HasherOutT<T>: TryFrom<[u8; 32]>,
    {
        let keys_to_delete: Vec<HasherOutT<T>> = self
            .bucket_prefix_map
            .iter()
            .filter_map(|full_key| {
                if full_key.starts_with(prefix) {
                    let key: [u8; 32] = full_key[32..].try_into().unwrap();
                    Some(
                        key.try_into()
                            .map_err(|_| FileStorageError::FailedToParseKey)
                            .unwrap(),
                    )
                } else {
                    None
                }
            })
            .collect();

        for key in keys_to_delete {
            self.metadata.remove(&key);
            self.file_data.remove(&key);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::H256;
    use sp_runtime::traits::BlakeTwo256;
    use sp_runtime::AccountId32;
    use sp_trie::LayoutV1;

    #[test]
    fn file_trie_create_empty_works() {
        let file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        // expected hash is the root hash of an empty tree.
        let expected_hash = HasherOutT::<LayoutV1<BlakeTwo256>>::try_from([
            3, 23, 10, 46, 117, 151, 183, 183, 227, 216, 76, 5, 57, 29, 19, 154, 98, 177, 87, 231,
            135, 134, 216, 192, 130, 242, 157, 207, 76, 17, 19, 20,
        ])
        .unwrap();

        assert_eq!(
            H256::from(*file_trie.get_root()),
            expected_hash,
            "Root should be initialized to default."
        );
    }

    #[test]
    fn file_trie_write_chunk_works() {
        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();
        let old_root = *file_trie.get_root();
        file_trie
            .write_chunk(&ChunkId::new(0u64), &Chunk::from([1u8; 1024]))
            .unwrap();
        let new_root = file_trie.get_root();
        assert_ne!(&old_root, new_root);

        let chunk = file_trie.get_chunk(&ChunkId::new(0u64)).unwrap();
        assert_eq!(chunk.as_slice(), [1u8; 1024]);
    }

    #[test]
    fn file_trie_get_chunk_works() {
        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        let chunk = Chunk::from([3u8; 32]);
        let chunk_id = ChunkId::new(3);
        file_trie.write_chunk(&chunk_id, &chunk).unwrap();
        let chunk = file_trie.get_chunk(&chunk_id).unwrap();
        assert_eq!(chunk.as_slice(), [3u8; 32]);
    }

    #[test]
    fn file_trie_stored_chunks_count_works() {
        let chunk_ids = vec![ChunkId::new(0u64), ChunkId::new(1u64)];
        let chunks = vec![Chunk::from([0u8; 1024]), Chunk::from([1u8; 1024])];
        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());
    }

    #[test]
    fn file_trie_generate_proof_works() {
        let chunk_ids = vec![ChunkId::new(0u64), ChunkId::new(1u64), ChunkId::new(2u64)];

        let chunks = vec![
            Chunk::from([0u8; 1024]),
            Chunk::from([1u8; 1024]),
            Chunk::from([2u8; 1024]),
        ];

        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());

        file_trie.write_chunk(&chunk_ids[2], &chunks[2]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 3);
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_ok());

        let file_proof = file_trie.generate_proof(&chunk_ids).unwrap();

        assert_eq!(
            file_proof.fingerprint.as_ref(),
            file_trie.get_root().as_ref()
        );
    }

    #[test]
    fn file_trie_delete_works() {
        let chunk_ids = vec![ChunkId::new(0u64), ChunkId::new(1u64), ChunkId::new(2u64)];

        let chunks = vec![
            Chunk::from([0u8; 1024]),
            Chunk::from([1u8; 1024]),
            Chunk::from([2u8; 1024]),
        ];

        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());

        file_trie.write_chunk(&chunk_ids[2], &chunks[2]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 3);
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_ok());

        file_trie.delete().unwrap();
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_err());
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_err());
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_err());

        assert_eq!(file_trie.stored_chunks_count().unwrap(), 0);
    }

    #[test]
    fn file_storage_insert_file_works() {
        let chunks = vec![
            Chunk::from([5u8; 32]),
            Chunk::from([6u8; 32]),
            Chunk::from([7u8; 32]),
        ];

        let chunk_ids: Vec<ChunkId> = chunks
            .iter()
            .enumerate()
            .map(|(id, _)| ChunkId::new(id as u64))
            .collect();

        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();

        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());

        file_trie.write_chunk(&chunk_ids[2], &chunks[2]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 3);
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_ok());

        let file_metadata = FileMetadata {
            file_size: 32u64 * chunks.len() as u64,
            fingerprint: file_trie.get_root().as_ref().into(),
            owner: <AccountId32 as AsRef<[u8]>>::as_ref(&AccountId32::new([0u8; 32])).to_vec(),
            location: "location".to_string().into_bytes(),
            bucket_id: [1u8; 32].to_vec(),
        };

        let key = file_metadata.file_key::<BlakeTwo256>();
        let mut file_storage = InMemoryFileStorage::<LayoutV1<BlakeTwo256>>::new();
        file_storage
            .insert_file_with_data(key, file_metadata, file_trie)
            .unwrap();

        assert!(file_storage.get_metadata(&key).is_ok());
        assert!(file_storage.get_chunk(&key, &chunk_ids[0]).is_ok());
        assert!(file_storage.get_chunk(&key, &chunk_ids[1]).is_ok());
        assert!(file_storage.get_chunk(&key, &chunk_ids[2]).is_ok());
    }

    #[test]
    fn file_storage_delete_file_works() {
        let chunks = vec![
            Chunk::from([5u8; 32]),
            Chunk::from([6u8; 32]),
            Chunk::from([7u8; 32]),
        ];

        let chunk_ids: Vec<ChunkId> = chunks
            .iter()
            .enumerate()
            .map(|(id, _)| ChunkId::new(id as u64))
            .collect();

        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();
        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());

        file_trie.write_chunk(&chunk_ids[2], &chunks[2]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 3);
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_ok());

        let file_metadata = FileMetadata {
            file_size: 32u64 * chunks.len() as u64,
            fingerprint: file_trie.get_root().as_ref().into(),
            owner: <AccountId32 as AsRef<[u8]>>::as_ref(&AccountId32::new([0u8; 32])).to_vec(),
            location: "location".to_string().into_bytes(),
            bucket_id: [1u8; 32].to_vec(),
        };

        let key = file_metadata.file_key::<BlakeTwo256>();
        let mut file_storage = InMemoryFileStorage::<LayoutV1<BlakeTwo256>>::new();
        file_storage
            .insert_file_with_data(key, file_metadata, file_trie)
            .unwrap();
        assert!(file_storage.get_metadata(&key).is_ok());

        assert!(file_storage.delete_file(&key).is_ok());

        // Should get a None option here when trying to get File Metadata.
        assert!(file_storage
            .get_metadata(&key)
            .is_ok_and(|metadata| metadata.is_none()));
        assert!(file_storage.get_chunk(&key, &chunk_ids[0]).is_err());
        assert!(file_storage.get_chunk(&key, &chunk_ids[1]).is_err());
        assert!(file_storage.get_chunk(&key, &chunk_ids[2]).is_err());
    }

    #[test]
    fn file_storage_generate_proof_works() {
        let chunks = vec![
            Chunk::from([0u8; 1024]),
            Chunk::from([1u8; 1024]),
            Chunk::from([2u8; 1024]),
        ];

        let chunk_ids: Vec<ChunkId> = chunks
            .iter()
            .enumerate()
            .map(|(id, _)| ChunkId::new(id as u64))
            .collect();

        let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();
        file_trie.write_chunk(&chunk_ids[0], &chunks[0]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 1);
        assert!(file_trie.get_chunk(&chunk_ids[0]).is_ok());

        file_trie.write_chunk(&chunk_ids[1], &chunks[1]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 2);
        assert!(file_trie.get_chunk(&chunk_ids[1]).is_ok());

        file_trie.write_chunk(&chunk_ids[2], &chunks[2]).unwrap();
        assert_eq!(file_trie.stored_chunks_count().unwrap(), 3);
        assert!(file_trie.get_chunk(&chunk_ids[2]).is_ok());

        let file_metadata = FileMetadata {
            file_size: 1024u64 * chunks.len() as u64,
            fingerprint: file_trie.get_root().as_ref().into(),
            owner: <AccountId32 as AsRef<[u8]>>::as_ref(&AccountId32::new([0u8; 32])).to_vec(),
            location: "location".to_string().into_bytes(),
            bucket_id: [1u8; 32].to_vec(),
        };

        let key = file_metadata.file_key::<BlakeTwo256>();
        let mut file_storage = InMemoryFileStorage::<LayoutV1<BlakeTwo256>>::new();

        file_storage
            .insert_file_with_data(key, file_metadata, file_trie)
            .unwrap();

        assert!(file_storage.get_metadata(&key).is_ok());

        let file_proof = file_storage.generate_proof(&key, &chunk_ids).unwrap();
        let proven_leaves = file_proof.proven::<LayoutV1<BlakeTwo256>>().unwrap();
        for (id, leaf) in proven_leaves.iter().enumerate() {
            assert_eq!(chunk_ids[id], leaf.key);
            assert_eq!(chunks[id], leaf.data);
        }
    }

    #[test]
    fn delete_files_with_prefix_works() {
        fn create_file_data_trie(
            chunks: &Vec<Chunk>,
        ) -> InMemoryFileDataTrie<LayoutV1<BlakeTwo256>> {
            let chunk_ids: Vec<ChunkId> = chunks
                .iter()
                .enumerate()
                .map(|(id, _)| ChunkId::new(id as u64))
                .collect();

            let mut file_trie = InMemoryFileDataTrie::<LayoutV1<BlakeTwo256>>::new();
            for (chunk_id, chunk) in chunk_ids.iter().zip(chunks) {
                file_trie.write_chunk(chunk_id, chunk).unwrap();
            }

            file_trie
        }

        fn create_file_metadata(
            file_trie: &InMemoryFileDataTrie<LayoutV1<BlakeTwo256>>,
            location: &str,
            bucket_id: [u8; 32],
        ) -> FileMetadata {
            FileMetadata {
                file_size: 1024u64 * 3,
                fingerprint: file_trie.get_root().as_ref().into(),
                owner: <AccountId32 as AsRef<[u8]>>::as_ref(&AccountId32::new([0u8; 32])).to_vec(),
                location: location.to_string().into_bytes(),
                bucket_id: bucket_id.to_vec(),
            }
        }

        let chunks = vec![
            Chunk::from([0u8; 1024]),
            Chunk::from([1u8; 1024]),
            Chunk::from([2u8; 1024]),
        ];

        let file_trie_1 = create_file_data_trie(&chunks);
        let file_metadata_1 = create_file_metadata(&file_trie_1, "location_1", [1u8; 32]);
        let file_key_1 = file_metadata_1.file_key::<BlakeTwo256>();

        let file_trie_2 = create_file_data_trie(&chunks);
        let file_metadata_2 = create_file_metadata(&file_trie_2, "location_2", [2u8; 32]);
        let file_key_2 = file_metadata_2.file_key::<BlakeTwo256>();

        let mut file_storage = InMemoryFileStorage::<LayoutV1<BlakeTwo256>>::new();

        file_storage
            .insert_file_with_data(file_key_1, file_metadata_1, file_trie_1)
            .unwrap();
        file_storage
            .insert_file_with_data(file_key_2, file_metadata_2, file_trie_2)
            .unwrap();

        assert!(file_storage.get_metadata(&file_key_1).is_ok());
        assert!(file_storage.get_metadata(&file_key_2).is_ok());

        let prefix = [1u8; 32].to_vec();
        file_storage
            .delete_files_with_prefix(prefix.as_slice().try_into().unwrap())
            .unwrap();

        assert!(file_storage
            .get_metadata(&file_key_1)
            .is_ok_and(|metadata| metadata.is_none()));
        assert!(file_storage
            .get_chunk(&file_key_1, &ChunkId::new(0u64))
            .is_err());
        assert!(file_storage
            .get_chunk(&file_key_1, &ChunkId::new(1u64))
            .is_err());
        assert!(file_storage
            .get_chunk(&file_key_1, &ChunkId::new(2u64))
            .is_err());

        assert!(file_storage.get_metadata(&file_key_2).is_ok());
        assert!(file_storage
            .get_chunk(&file_key_2, &ChunkId::new(0u64))
            .is_ok());
        assert!(file_storage
            .get_chunk(&file_key_2, &ChunkId::new(1u64))
            .is_ok());
        assert!(file_storage
            .get_chunk(&file_key_2, &ChunkId::new(2u64))
            .is_ok());
    }
}
