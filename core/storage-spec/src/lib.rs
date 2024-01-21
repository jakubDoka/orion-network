#![feature(slice_take)]
#![feature(array_windows)]
//use std::collections::HashMap;

mod berkleamp_wench;
pub mod db;

pub use berkleamp_wench::Fec;

//pub const SHARD_SIZE_BYTES: usize = 1024 * 4;
//pub const SHARD_SIZE: usize = SHARD_SIZE_BYTES / std::mem::size_of::<Elem>();
//pub const DATA_SHARDS: usize = if cfg!(debug_assertions) { 4 } else { 16 };
//pub const PARITY_SHARDS: usize = if cfg!(debug_assertions) { 4 } else { 16 };
//pub const MAX_SHARDS: usize = DATA_SHARDS + PARITY_SHARDS;
//pub const SHARD_SECURITY: usize = 32;
//pub const BLOCK_SIZE: usize = 1024 * 1024 * 4;
//pub const BLOCK_SHARDS: usize = BLOCK_SIZE / SHARD_SIZE_BYTES;
//pub const SHARD_PROOF_SIZE: usize = BLOCK_SHARDS.ilog2() as usize;
//
//pub type Result<T> = std::result::Result<T, reed_solomon_erasure::Error>;
//pub type Shard = [Elem; SHARD_SIZE];
//pub type DataShards = [Shard; DATA_SHARDS];
//pub type ParityShards = [Shard; PARITY_SHARDS];
//pub type AllShards = [([Elem; SHARD_SIZE], bool); DATA_SHARDS];
//pub type ShardHash = [u8; SHARD_SECURITY];
//pub type NodeIdentity = [u8; 32];
//pub type NodeId = u32; // we will reuse ids
//pub type Group = [NodeId; MAX_SHARDS];
//pub type FileAddress = [u8; 32]; // blake3 hash of the file
//pub type ShardIndex = u8;
//pub type FileTempSignKey = crypto::sign::Keypair;
//pub type FileVerifyKey = crypto::sign::PublicKey;
//pub type BlockSignature = crypto::sign::Signature;
//pub type ShardProof = [ShardHash; SHARD_PROOF_SIZE];
//
//type Field = reed_solomon_erasure::galois_8::Field;
//type Elem = <Field as reed_solomon_erasure::Field>::Elem;
//
//#[derive(Clone, Copy)]
//pub struct FileMeta {
//    pub shard_count: u64,
//    pub verification_key: FileVerifyKey,
//}
//
//#[derive(Clone, Copy)]
//pub struct BlockMeta {
//    pub group: Group,
//}
//
//pub fn shard_as_bytes(shard: &Shard) -> &[u8; SHARD_SIZE * std::mem::size_of::<Elem>()] {
//    unsafe { std::mem::transmute(shard) }
//}
//
///// if the shard is valid, return the index of the shard
//pub fn validate_shard(hash: &ShardHash, shard: &Shard) -> Option<ShardIndex> {
//    let mut hasher = blake3::Hasher::new();
//    hasher.update(shard_as_bytes(shard));
//    (0..MAX_SHARDS as ShardIndex).find(|&i| {
//        let mut hasher = hasher.clone();
//        hasher.update(&[i]);
//        &hasher.finalize().as_bytes()[..SHARD_SECURITY] == hash
//    })
//}
//
///// See `validate_shard`
//pub fn hash_shard(index: ShardIndex, shard: &Shard) -> ShardHash {
//    let mut hasher = blake3::Hasher::new();
//    hasher.update(shard_as_bytes(shard));
//    hasher.update(&[index]);
//    hasher.finalize().as_bytes()[..SHARD_SECURITY].try_into().unwrap()
//}
//
//pub struct Codec {
//    inner: reed_solomon_erasure::ReedSolomon<Field>,
//}
//
//impl Default for Codec {
//    fn default() -> Self {
//        let inner = reed_solomon_erasure::ReedSolomon::new(DATA_SHARDS, PARITY_SHARDS).unwrap();
//        Self { inner }
//    }
//}
//
//impl Codec {
//    pub fn encode(&self, data: &mut DataShards, parity: &mut ParityShards) -> Result<()> {
//        self.inner.encode_sep(data, parity)
//    }
//
//    pub fn reconstruct(&self, shards: &mut AllShards) -> Result<()> {
//        self.inner.reconstruct_data(shards)
//    }
//}
//
//#[cfg(test)]
//mod test {
//    use reed_solomon_erasure::galois_8::ReedSolomon;
//
//    #[test]
//    fn test_encode() {
//        let rs = ReedSolomon::new(3, 2).unwrap();
//
//        let mut data = [[1, 2], [3, 4], [5, 6], [0, 0], [0, 0]];
//
//        rs.encode(&mut data).unwrap();
//
//        //data.swap(0, 3);
//
//        assert!(rs.verify(&data).unwrap());
//    }
//}
