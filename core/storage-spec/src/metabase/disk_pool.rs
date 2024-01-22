use {
    crate::{sorted_compact_vec::SortedCompactVec, BlockId},
    component_utils::Codec,
    std::{io, iter, ops::Range, os::unix::fs::FileExt, path::Path, usize},
};

const CONTEXT_FILE_NAME: &str = "context.dp";
const GROUP_FILE_NAME_PREFIX: &str = "group";

const FILE_COUNT: usize = 1024;

/// # Safety
/// The type needs to be safely transmutabe to array of bytes.
pub unsafe trait Record: Sized {
    const EXT: &'static str;

    fn as_bytes(slice: &[Self]) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                slice.as_ptr() as *const u8,
                slice.len() * Self::record_size(),
            )
        }
    }
    fn as_bytes_mut(slice: &mut [Self]) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                slice.as_mut_ptr() as *mut u8,
                slice.len() * Self::record_size(),
            )
        }
    }

    fn file_capacity() -> usize {
        (1 << (32 - FILE_COUNT.ilog2())) * Self::record_size()
    }

    fn record_size() -> usize {
        std::mem::size_of::<Self>()
    }

    fn id_to_offsets(id: BlockId, count: usize) -> (usize, usize, usize) {
        let file_index = (id as usize * Self::record_size()) / Self::file_capacity();
        let file_offset = (id as usize * Self::record_size()) % Self::file_capacity();
        let rw_len =
            std::cmp::min(count * Self::record_size(), Self::file_capacity() - file_offset);
        (file_index, file_offset, rw_len / Self::record_size())
    }
}

#[derive(Codec, Default)]
pub struct Context {
    free_blocks: SortedCompactVec,
}

impl Context {
    pub fn open(root_dir: &Path) -> io::Result<Self> {
        let full_path = root_dir.join(CONTEXT_FILE_NAME);

        if !full_path.exists() {
            return Ok(Self::default());
        }

        let bytes = std::fs::read(full_path)?;

        Self::decode(&mut bytes.as_slice()).ok_or(io::ErrorKind::InvalidData.into())
    }

    pub fn save(&self, root_dir: &Path) -> io::Result<()> {
        let bytes = self.to_bytes();
        let full_path = root_dir.join(CONTEXT_FILE_NAME);
        std::fs::write(full_path, bytes)
    }
}

struct FileAccess {
    fd: std::fs::File,
    //write_nonce: AtomicUsize,
}

pub struct Db<T> {
    files: Box<[FileAccess]>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: Record> Db<T> {
    pub fn open(root_dir: &Path) -> std::io::Result<Self> {
        let files = (0..FILE_COUNT)
            .map(|i| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(root_dir.join(format!("{GROUP_FILE_NAME_PREFIX}{i}.{}", T::EXT,)))
                    .map(|fd| FileAccess { fd /* write_nonce: AtomicUsize::new(0) */ })
            })
            .collect::<Result<Box<[_]>, _>>()?;

        Ok(Self { files, _marker: std::marker::PhantomData })
    }

    pub fn fetch<'a>(&self, start: BlockId, buffer: &mut &'a mut [T]) -> io::Result<&'a mut [T]> {
        let (file_index, file_offset, read_len) = T::id_to_offsets(start, buffer.len());
        let file = &self.files.get(file_index).ok_or(io::ErrorKind::NotFound)?;
        let to_read = buffer.take_mut(..read_len).unwrap();
        file.fd.read_exact_at(T::as_bytes_mut(to_read), file_offset as u64)?;
        //Ok((to_read, file.write_nonce.load(std::sync::atomic::Ordering::Relaxed)))
        Ok(to_read)
    }

    pub fn fetch_iter<'a>(
        &'a self,
        start: BlockId,
        mut buffer: &'a mut [T],
    ) -> impl Iterator<Item = io::Result<&'a mut [T]>> + 'a {
        iter::from_fn(move || {
            if buffer.is_empty() {
                return None;
            }

            Some(self.fetch(start, &mut buffer))
        })
    }

    // pub fn check_write_id(&self, id: BlockId) -> usize {
    //     let (file_index, ..) = T::id_to_offsets(id, 1);
    //     self.files[file_index].write_nonce.load(std::sync::atomic::Ordering::Relaxed)
    // }

    pub fn push(&self, block: &mut &[T], context: &mut Context) -> io::Result<Range<BlockId>> {
        let range = context.free_blocks.pop_n(block.len()).ok_or(io::ErrorKind::OutOfMemory)?;
        let (file_index, file_offset, write_len) = T::id_to_offsets(range.start, block.len());
        let file = &self.files.get(file_index).ok_or(io::ErrorKind::NotFound)?;

        let to_write = block.take(..write_len).expect("it can only be less");
        file.fd.write_all_at(T::as_bytes(to_write), file_offset as u64)?;
        // if Some(range.start) != context.free_blocks.lowest_active() {
        //     file.write_nonce.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // }
        Ok(range)
    }

    pub fn push_iter<'a>(
        &'a self,
        mut block: &'a [T],
        context: &'a mut Context,
    ) -> impl Iterator<Item = io::Result<Range<BlockId>>> + 'a {
        iter::from_fn(move || {
            if block.is_empty() {
                return None;
            }

            Some(self.push(&mut block, context))
        })
    }

    pub fn release(&self, range: Range<BlockId>, context: &mut Context) {
        context.free_blocks.push_range(range);
    }

    pub fn reallocate(&self, range: Range<BlockId>, context: &mut Context) -> io::Result<Range<BlockId>> {
        let new_range
    }
}
