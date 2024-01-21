#![feature(slice_take)]
#![feature(array_windows)]
#![feature(slice_flatten)]
use std::collections::HashMap;

pub mod db;
pub mod protocol;

pub const SHARD_SIZE_BYTES: usize = 1024 * 4;
pub const SHARD_SIZE: usize = SHARD_SIZE_BYTES / std::mem::size_of::<Elem>();
pub const DATA_SHARDS: usize = if cfg!(debug_assertions) { 4 } else { 16 };
pub const PARITY_SHARDS: usize = if cfg!(debug_assertions) { 4 } else { 16 };
pub const MAX_SHARDS: usize = DATA_SHARDS + PARITY_SHARDS;
pub const SHARD_SECURITY: usize = 32;
pub const BLOCK_SIZE: usize = 1024 * 1024 * 4;
pub const BLOCK_SHARDS: usize = BLOCK_SIZE / SHARD_SIZE_BYTES;
pub const SHARD_PROOF_SIZE: usize = BLOCK_SHARDS.ilog2() as usize;

pub type ReconstructBundle<'data> = [ReconstructShard<'data>; DATA_SHARDS];
pub type Shard = [Elem; SHARD_SIZE];
pub type DataShards = [Shard; DATA_SHARDS];
pub type ParityShards = [Shard; PARITY_SHARDS];
pub type AllShards = [([Elem; SHARD_SIZE], bool); DATA_SHARDS];
pub type ShardHash = [u8; SHARD_SECURITY];
pub type NodeIdentity = [u8; 32];
pub type NodeId = u32; // we will reuse ids
pub type Group = [NodeId; MAX_SHARDS];
pub type FileAddress = [u8; 32]; // blake3 hash of the file
pub type ShardIndex = u8;
pub type ShardProof = [ShardHash; SHARD_PROOF_SIZE];

type Elem = u8;

pub use {
    berkleamp_welch::Share as ReconstructShard,
    crypto::sign::{
        Keypair as FileTempSignKey, PublicKey as FileVerifyKey, Signature as BlockSignature,
    },
};

#[derive(Clone, Copy)]
pub struct FileMeta {
    pub shard_count: u64,
    pub verification_key: FileVerifyKey,
}

#[derive(Clone, Copy)]
pub struct BlockMeta {
    pub group: Group,
}

#[must_use]
pub fn shard_as_bytes(shard: &Shard) -> &[u8; SHARD_SIZE * std::mem::size_of::<Elem>()] {
    unsafe { std::mem::transmute(shard) }
}

/// if the shard is valid, return the index of the shard
#[must_use]
pub fn validate_shard(hash: &ShardHash, shard: &Shard) -> Option<ShardIndex> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(shard_as_bytes(shard));
    (0..MAX_SHARDS as Elem).find(|&i| {
        let mut hasher = hasher.clone();
        hasher.update(&[i]);
        &hasher.finalize().as_bytes()[..SHARD_SECURITY] == hash
    })
}

/// See `validate_shard`
#[must_use]
pub fn hash_shard(index: ShardIndex, shard: &Shard) -> ShardHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(shard_as_bytes(shard));
    hasher.update(&[index]);
    hasher.finalize().as_bytes()[..SHARD_SECURITY].try_into().unwrap()
}

pub struct Codec {
    inner: berkleamp_welch::Fec,
    buffer: Vec<u8>,
}

impl Default for Codec {
    fn default() -> Self {
        Self { inner: berkleamp_welch::Fec::new(DATA_SHARDS, PARITY_SHARDS), buffer: vec![] }
    }
}

impl Codec {
    pub fn encode(&self, data: &DataShards, parity: &mut ParityShards) {
        self.inner.encode(data.flatten(), parity.flatten_mut()).unwrap();
    }

    pub fn reconstruct(
        &mut self,
        shards: &mut ReconstructBundle,
    ) -> Result<(), berkleamp_welch::RebuildError> {
        self.inner.rebuild(shards, &mut self.buffer).map(drop)
    }
}
