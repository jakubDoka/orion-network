use super::{BlockMeta, FileAddress, FileMeta, HashMap};

pub trait Database {
    fn get(&self, address: &FileAddress) -> Option<FileMeta>;
    fn get_block(&self, address: &FileAddress, index: u64) -> Option<BlockMeta>;
    fn get_block_stream_from(
        &self,
        address: &FileAddress,
        index: u64,
    ) -> impl Iterator<Item = BlockMeta>;

    fn put(
        &mut self,
        address: FileAddress,
        meta: FileMeta,
        blocks: impl IntoIterator<Item = BlockMeta>,
    );

    fn delete(&mut self, address: &FileAddress);
}

#[derive(Default)]
pub struct MemoryDatabase {
    files: HashMap<FileAddress, (FileMeta, Vec<BlockMeta>)>,
}

impl Database for MemoryDatabase {
    fn get(&self, address: &FileAddress) -> Option<FileMeta> {
        self.files.get(address).map(|&(meta, _)| meta)
    }

    fn get_block(&self, address: &FileAddress, index: u64) -> Option<BlockMeta> {
        self.files.get(address).and_then(|(_, blocks)| blocks.get(index as usize).copied())
    }

    fn get_block_stream_from(
        &self,
        address: &FileAddress,
        index: u64,
    ) -> impl Iterator<Item = BlockMeta> {
        self.files
            .get(address)
            .map(|(_, blocks)| blocks.iter().skip(index as usize).copied())
            .into_iter()
            .flatten()
    }

    fn put(
        &mut self,
        address: FileAddress,
        meta: FileMeta,
        blocks: impl IntoIterator<Item = BlockMeta>,
    ) {
        self.files.entry(address).or_insert((meta, vec![])).1.extend(blocks);
    }

    fn delete(&mut self, address: &FileAddress) {
        self.files.remove(address);
    }
}
