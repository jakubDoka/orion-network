use std::{collections::VecDeque, iter, num::NonZeroUsize, usize};

use component_utils::{libp2p_identity::PeerId, Codec};

pub const CHAT_NAME_CAP: usize = 32;
pub const IDENTITY_CAP: usize = 32;
pub const CHAT_CAP: usize = 1024 * 1024;
pub const MAX_MESSAGE_SIZE: u16 = 1024;
pub const NO_CURSOR: Cursor = Cursor::MAX;
pub const PROTO_NAME: &str = "orion-chat";
pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(3) };

pub type ChatName = [u8; CHAT_NAME_CAP];
pub type Cursor = u32;
pub type Permission = u32;
pub type Identity = crypto::sign::SerializedPublicKey;

pub struct UserKeys {
    sign: crypto::sign::KeyPair,
    enc: crypto::enc::KeyPair,
}

crypto::impl_transmute! {
    UserKeys, USER_KEYS_SIZE, SerializedUserKeys;
}

pub fn string_to_chat_name(str: &str) -> Option<ChatName> {
    if str.len() > CHAT_NAME_CAP {
        return None;
    }

    let mut res = [0; CHAT_NAME_CAP];

    res[..str.len()].copy_from_slice(str.as_bytes());

    Some(res)
}

pub fn chat_name_to_string(name: &ChatName) -> Option<&str> {
    let len = CHAT_NAME_CAP - name.iter().rev().take_while(|&&b| b == 0).count();
    std::str::from_utf8(&name[..len]).ok()
}

component_utils::protocol! { 'a:
    #[derive(Clone, Copy)]
    enum Request<'a> {
        SearchFor: ChatName => 0,
        Subscribe: ChatName => 1,
        Send: Message<'a> => 2,
        FetchMessages: FetchMessages => 3,
        KeepAlive: () => 4,
    }

    #[derive(Clone, Copy)]
    struct Message<'a> {
        chat: ChatName,
        content: &'a [u8],
        content_sig: crypto::sign::SerializedSignature,
        sender: Identity,
    }

    #[derive(Clone, Copy)]
    struct FetchMessages {
        chat: ChatName,
        cursor: Cursor,
    }

    #[derive(Clone, Copy)]
    struct AddMember {
        chat: ChatName,
        invited: Identity,
        perm_diff: Permission,
        invited_sig: crypto::sign::SerializedSignature,
        sender: Identity,
    }

    enum Response<'a> {
        Message: Message<'a> => 1,
        FetchedMessages: FetchedMessages<'a> => 3,
        SearchResults: SearchResult => 4,
        Subscribed: ChatName => 5,
        NotFound: () => 6,
    }

    struct SearchResult {
        members: Vec<PeerId>,
        chat: ChatName,
    }

    #[derive(Clone, Copy)]
    struct FetchedMessages<'a> {
        chat: ChatName,
        cursor: Cursor,
        messages: &'a [u8],
    }

    #[derive(Clone, Copy)]
    enum PutRecord<'a> {
        Message: &'a [u8] => 1,
        ChatHistory: ChatHistory<'a> => 2,
    }

    #[derive(Clone, Copy)]
    struct ReplicateMessage<'a> {
        message: &'a [u8],
        content_sig: crypto::sign::SerializedSignature,
        sender: Identity,
    }

    #[derive(Clone, Copy)]
    struct ChatHistory<'a> {
        offset: Cursor,
        first: &'a [u8],
        last: &'a [u8],
    }
}

impl<'a> FetchedMessages<'a> {
    pub fn messages(&self) -> impl Iterator<Item = &'a [u8]> {
        let mut iter = self.messages.iter();
        iter::from_fn(move || {
            let len = iter
                .by_ref()
                .copied()
                .next_chunk()
                .map(u16::from_be_bytes)
                .ok()?;

            let slice = iter.as_slice().get(..len as usize)?;
            iter.advance_by(len as usize).unwrap();

            Some(slice)
        })
    }
}

impl<'a> ChatHistory<'a> {
    pub fn to_blob(self) -> MessageBlob {
        MessageBlob {
            data: self.first.iter().chain(self.first).copied().collect(),
            offset: self.offset,
        }
    }
}

#[derive(Default)]
pub struct MessageBlob {
    data: VecDeque<u8>,
    offset: Cursor,
}

impl MessageBlob {
    pub fn push(&mut self, bytes: &[u8], chat_cap: usize) {
        if self.data.len() + bytes.len() > chat_cap {
            self.pop();
        }

        let len = bytes.len() as u16;
        self.offset += len as Cursor + 4;
        self.data.extend(len.to_be_bytes());
        self.data.extend(bytes);
        self.data.extend(len.to_be_bytes());
    }

    pub fn as_vec(&self) -> Vec<u8> {
        let (first, last) = self.data.as_slices();
        let packet = PutRecord::ChatHistory(ChatHistory {
            offset: self.offset,
            first,
            last,
        });
        packet.to_bytes()
    }

    pub fn pop(&mut self) {
        let Ok(header) = self
            .data
            .iter()
            .copied()
            .next_chunk()
            .map(u16::from_be_bytes)
        else {
            return;
        };

        self.data.drain(..header as usize + 4);
    }

    pub fn fetch(
        &self,
        mut cursor: Cursor,
        limit: usize,
        message_limit: u16,
        buffer: &mut Vec<u8>,
    ) -> Cursor {
        // cursor can be invalid so code does not assume anithing
        // complexity should only decrease if cursor is invalid
        cursor = cursor.min(self.offset);

        let to_skip = (self.offset - cursor) as usize;
        let mut iter = self.data.iter().rev();
        if iter.advance_by(to_skip).is_err() {
            return cursor;
        }

        for _ in 0..limit {
            // we use le since we are reversed
            let Ok(header) = iter.by_ref().copied().next_chunk().map(u16::from_le_bytes) else {
                cursor = NO_CURSOR;
                break;
            };

            if header > message_limit {
                cursor = NO_CURSOR;
                break;
            }

            buffer.extend(header.to_be_bytes());
            buffer.extend(iter.clone().take(header as usize).rev());
            _ = iter.advance_by(header as usize + 2);
            cursor -= header as Cursor + 4;
        }

        cursor
    }

    pub fn try_replace(&mut self, c: ChatHistory<'_>) {
        if c.offset < self.offset || c.first.len() + c.last.len() <= self.data.len() {
            return;
        }

        self.data.clear();
        self.data.extend(c.first);
        self.data.extend(c.last);
        self.offset = c.offset;
    }
}

#[cfg(test)]
mod test {
    use crate::chat::CHAT_CAP;

    #[test]
    fn test_push() {
        let mut blob = super::MessageBlob::default();

        blob.push(b"hello", CHAT_CAP);
        blob.push(b"world", CHAT_CAP);

        assert_eq!(
            blob.data,
            vec![
                0, 5, b'h', b'e', b'l', b'l', b'o', 0, 5, 0, 5, b'w', b'o', b'r', b'l', b'd', 0, 5
            ]
        );

        blob.pop();

        assert_eq!(blob.data, vec![0, 5, b'w', b'o', b'r', b'l', b'd', 0, 5]);
    }

    #[test]
    fn test_fetch() {
        let mut blob = super::MessageBlob::default();

        for i in 0..10 {
            blob.push(&[i, i + 1], CHAT_CAP);
        }

        let mut buffer = Vec::new();
        let mut cursor = super::NO_CURSOR;

        cursor = blob.fetch(cursor, 2, 2, &mut buffer);

        assert_eq!(buffer, vec![0, 2, 9, 10, 0, 2, 8, 9]);
        assert_eq!(cursor, 48);

        buffer.clear();
        cursor = blob.fetch(cursor, 2, 2, &mut buffer);

        assert_eq!(buffer, vec![0, 2, 7, 8, 0, 2, 6, 7]);
        assert_eq!(cursor, 36);
    }
}
