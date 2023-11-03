use std::{borrow::Cow, collections::VecDeque, fmt, iter, num::NonZeroUsize, usize};

use component_utils::{libp2p_identity::PeerId, Codec};

pub const CHAT_NAME_CAP: usize = 32;
pub const IDENTITY_CAP: usize = 32;
pub const CHAT_CAP: usize = 1024 * 1024;
pub const MAX_MESSAGE_SIZE: u16 = 1024;
pub const NO_CURSOR: Cursor = Cursor::MAX;
pub const PROTO_NAME: &str = "orion-chat";
pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(3) };

pub type Cursor = u32;
pub type Permission = u32;
pub type MemberId = u32;
pub type Identity = crypto::sign::SerializedPublicKey;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ChatName {
    repr: [u8; CHAT_NAME_CAP],
}

impl<'a> Codec<'a> for ChatName {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.extend(self.repr);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(Self {
            repr: <_>::decode(buffer)?,
        })
    }
}

impl From<[u8; CHAT_NAME_CAP]> for ChatName {
    fn from(repr: [u8; CHAT_NAME_CAP]) -> Self {
        Self { repr }
    }
}

impl ChatName {
    pub fn new(str: &str) -> Option<Self> {
        if str.len() > CHAT_NAME_CAP {
            return None;
        }
        let mut repr = [0; CHAT_NAME_CAP];
        repr[..str.len()].copy_from_slice(str.as_bytes());
        Some(Self { repr })
    }

    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(
            &self.repr[..CHAT_NAME_CAP - self.repr.iter().rev().take_while(|&&b| b == 0).count()],
        )
        .ok()
    }

    pub fn as_hex(&self) -> String {
        hex::encode(self.repr)
    }

    pub fn as_string(&self) -> Cow<'_, str> {
        match self.as_str() {
            Some(str) => Cow::Borrowed(str),
            None => Cow::Owned(self.as_hex()),
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.repr.to_vec()
    }
}

impl fmt::Display for ChatName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_string().fmt(f)
    }
}

pub struct UserKeys {
    pub sign: crypto::sign::KeyPair,
    pub enc: crypto::enc::KeyPair,
}

impl UserKeys {
    pub fn new() -> Self {
        Self {
            sign: crypto::sign::KeyPair::new(),
            enc: crypto::enc::KeyPair::new(),
        }
    }

    pub fn identity(&self) -> UserIdentity {
        UserIdentity {
            sign: self.sign.public_key(),
            enc: self.enc.public_key(),
        }
    }
}

impl Default for UserKeys {
    fn default() -> Self {
        Self::new()
    }
}

pub struct UserIdentity {
    pub sign: crypto::sign::PublicKey,
    pub enc: crypto::enc::PublicKey,
}

crypto::impl_transmute! {
    UserKeys, USER_KEYS_SIZE, SerializedUserKeys;
    UserIdentity, USER_IDENTITY_SIZE, SerializedUserIdentity;
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[repr(u8)]
pub enum PutMessageError {
    #[error("cannot parse message content")]
    InvalidContent,
    #[error("message signature does not check out")]
    InvalidMessage,
    #[error("you are not a member of this chat")]
    NotMember,
    #[error("message number is too low")]
    MessageNumberTooLow,
    #[error("you are not permitted to do this")]
    NotPermitted,
    #[error("member not found")]
    MemberNotFound,
}

impl<'a> Codec<'a> for PutMessageError {
    fn encode(&self, buffer: &mut Vec<u8>) {
        buffer.push(*self as u8);
    }

    fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
        Some(match u8::decode(buffer)? {
            0 => Self::InvalidContent,
            1 => Self::InvalidMessage,
            2 => Self::NotMember,
            3 => Self::MessageNumberTooLow,
            4 => Self::NotPermitted,
            5 => Self::MemberNotFound,
            _ => return None,
        })
    }
}

component_utils::protocol! { 'a:
    #[derive(Clone, Copy)]
    enum Request<'a> {
        SearchFor: ChatName => 0,
        Subscribe: ChatName => 1,
        Send: Message<'a> => 2,
        FetchMessages: FetchMessages => 3,
        KeepAlive => 4,
    }

    #[derive(Clone, Copy)]
    struct PrefixedMessage<'a> {
        no: u32,
        content: &'a [u8],
    }

    #[derive(Clone, Copy)]
    enum MessageContent<'a> {
        Arbitrary: &'a [u8] => 0,
        AddMember: AddMember => 1,
        RemoveMember: MemberId => 2,
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
        invited: Identity,
        perm_offset: u32,
    }

    enum Response<'a> {
        Message: Message<'a> => 1,
        FetchedMessages: FetchedMessages<'a> => 3,
        SearchResults: SearchResult => 4,
        Subscribed: ChatName => 5,
        FailedMessage: PutMessageError => 6,
        ChatNotFound => 7,
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
        Message: ReplicateMessage<'a> => 1,
        ChatHistory: ChatHistory<'a> => 2,
    }

    #[derive(Clone, Copy)]
    struct ReplicateMessage<'a> {
        content: &'a [u8],
        content_sig: crypto::sign::SerializedSignature,
        sender: Identity,
    }

    #[derive(Clone, Copy)]
    struct ChatHistory<'a> {
        offset: Cursor,
        first: &'a [u8],
        last: &'a [u8],
    }

    #[derive(Clone, Copy)]
    struct Member {
        id: MemberId,
        identity: Identity,
        perm: Permission,
        last_message_no: u32,
    }
}

impl<'a> ReplicateMessage<'a> {
    pub fn is_valid(&self) -> bool {
        crypto::sign::PublicKey::from(self.sender)
            .verify(self.content, &self.content_sig.into())
            .is_ok()
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
    pub fn push<I: IntoIterator<Item = u8>>(&mut self, bytes: I, chat_cap: usize) {
        let prev_len = self.data.len();
        let mut iter = 0u16
            .to_be_bytes()
            .into_iter()
            .chain(bytes)
            .chain(0u16.to_be_bytes());

        let mut remining = chat_cap - self.data.len();
        while let Some(byte) = iter.next() {
            if remining == 0 {
                self.pop();
                remining = chat_cap - self.data.len() - 1;
            }
            self.data.push_back(byte);
            self.data.extend(iter.by_ref().take(remining));
        }

        let len = self.data.len() - prev_len - 4;
        self.data
            .iter_mut()
            .skip(prev_len)
            .zip(len.to_be_bytes())
            .for_each(|(a, b)| *a = b);
        self.data
            .iter_mut()
            .rev()
            .zip(len.to_le_bytes())
            .for_each(|(a, b)| *a = b);
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

        blob.push(b"hello".iter().cloned(), CHAT_CAP);
        blob.push(b"world".iter().cloned(), CHAT_CAP);

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
            blob.push([i, i + 1], CHAT_CAP);
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
