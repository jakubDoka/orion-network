use std::{collections::VecDeque, iter, num::NonZeroUsize, usize};
use std::{mem, u16, u32};

use component_utils::arrayvec::ArrayString;
use component_utils::{libp2p_identity::PeerId, Codec};

pub const CHAT_NAME_CAP: usize = 32;
pub const USER_NAME_CAP: usize = 32;
pub const CHAT_CAP: usize = 1024 * 1024;
pub const MAIL_CAP: usize = 1024 * 1024;
pub const USER_DATA_CAP: usize = 1024 * 1024;
pub const MAX_MESSAGE_SIZE: usize = 1024;
pub const MAX_MAIL_SIZE: usize = 512;
pub const MESSAGE_FETCH_LIMIT: usize = 20;
pub const NO_CURSOR: Cursor = Cursor::MAX;
pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(3) };

pub type Cursor = u32;
pub type Permission = u32;
pub type MemberId = u32;
pub type Identity = crypto::sign::SerializedPublicKey;
pub type ChatName = ArrayString<CHAT_NAME_CAP>;
pub type UserName = ArrayString<USER_NAME_CAP>;
pub type UserMailId = crypto::sign::SerializedPublicKey;

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

macro_rules! gen_simple_error {
    ($(
        error $name:ident {$(
            $variant:ident => $message:literal,
        )*}
    )*) => {$(
        #[derive(Debug, Clone, Copy, thiserror::Error)]
        #[repr(u8)]
        pub enum $name {$(
            #[error($message)]
            $variant,
        )*}


        impl<'a> Codec<'a> for $name {
            fn encode(&self, buffer: &mut Vec<u8>) {
                buffer.push(*self as u8);
            }

            fn decode(buffer: &mut &'a [u8]) -> Option<Self> {
                let max_var = [$(Self::$variant),*].len();
                let b = u8::decode(buffer)?;
                if b >= max_var as u8 {
                    return None;
                }
                Some(unsafe { std::mem::transmute(b) })
            }
        }
    )*};
}

gen_simple_error! {
    error PutMessageError {
        InvalidContent => "cannot parse message content",
        InvalidMessage => "message signature does not check out",
        NotMember => "you are not a member of this chat",
        MessageNumberTooLow => "message number is too low",
        NotPermitted => "you are not permitted to do this",
        MemberNotFound => "member not found",
        MessageTooBig => "message is too big",
    }

    error PutMailError {
        MailboxFull => "user's mail box is full (they don't care about you)",
        MailTooBig => "one mail has limmited size ({MAX_MAIL_SIZE}), you excided it",
    }

    error ReadMailError {
        NotPermitted => "you are not permitted to do this",
        InvalidProof => "your proof is weak, no",
    }

    error WriteDataError {
        NotPermitted => "not gonna happen (permission denaid)",
        InvalidProof => "send me a proof, not random bytes (invalid proof)",
    }
}

impl MailActionProof {
    pub fn is_valid(self) -> bool {
        let Self { pk, no, sig } = self;
        const ID: &[u8] = b"mail-action";
        let mut msg = [0; ID.len() + 4];
        msg[..ID.len()].copy_from_slice(ID);
        msg[ID.len()..].copy_from_slice(&no.to_le_bytes());

        crypto::sign::PublicKey::from(pk)
            .verify(&msg, &sig.into())
            .is_ok()
    }
}

impl UserOrChat {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::User(pk) => pk.as_ref(),
            Self::Chat(name) => name.as_bytes(),
        }
    }
}

component_utils::protocol! { 'a:
    #[derive(Clone, Copy)]
    enum Request<'a> {
        SearchFor: UserOrChat => 0,
        Subscribe: ChatName => 1,
        Send: Message<'a> => 2,
        FetchMessages: FetchMessages => 3,
        KeepAlive => 4,
        ReadMail: MailActionProof => 5,
        WriteData: WriteData<'a> => 6,
        ReadData: UserMailId => 7,
        WriteMail: WriteMail<'a> => 8,
    }

    #[derive(Clone, Copy)]
    enum UserOrChat {
        User: UserMailId => 0,
        Chat: ChatName => 1,
    }

    #[derive(Clone, Copy)]
    struct MailActionProof {
        pk: UserMailId,
        no: u32,
        sig: crypto::sign::SerializedSignature,
    }

    #[derive(Clone, Copy)]
    struct WriteData<'a> {
        data: &'a [u8],
        proof: MailActionProof,
    }

    #[derive(Clone, Copy)]
    struct WriteMail<'a> {
        data: &'a [u8],
        id: UserMailId,
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
        DataRed: &'a [u8] => 8,
        MailWritten => 9,
        MailWriteFailed: PutMailError => 10,
    }

    struct SearchResult {
        members: Vec<PeerId>,
        chat: UserOrChat,
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
        Mail: &'a [u8] => 2,
        ChatHistory: ChatHistory<'a> => 2,
    }

    #[derive(Clone, Copy)]
    enum Mail {
        ChatInvite: ChatName => 0,
    }

    #[derive(Clone, Copy)]
    struct ChatInvite {
        chat: ChatName,
        member_id: MemberId,
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
pub struct MailBlob {
    data: Vec<u8>,
}

impl MailBlob {
    pub fn push<I: IntoIterator<Item = u8>>(&mut self, bytes: I) -> bool {
        let prev_len = self.data.len();
        let mut iter = 0u16.to_be_bytes().into_iter().chain(bytes);

        self.data
            .extend(iter.by_ref().take(MAIL_CAP - self.data.len()));

        let suc = iter.next().is_none();
        if !suc {
            self.data.truncate(prev_len);
        } else {
            let len = (self.data.len() - prev_len - 2) as u16;
            self.data
                .iter_mut()
                .skip(prev_len)
                .zip(len.to_be_bytes())
                .for_each(|(a, b)| *a = b);
        }
        suc
    }

    pub fn read(&mut self) -> &mut [u8] {
        // SAFETY: the truncation serves as cleanup mechanism
        let slice = unsafe { mem::transmute(self.data.as_mut_slice()) };
        unsafe { self.data.set_len(0) }
        slice
    }
}

#[derive(Default)]
pub struct MessageBlob {
    data: VecDeque<u8>,
    offset: Cursor,
}

impl MessageBlob {
    pub fn push<I: IntoIterator<Item = u8>>(&mut self, bytes: I) {
        let prev_len = self.data.len();
        let mut iter = 0u16
            .to_be_bytes()
            .into_iter()
            .chain(bytes)
            .chain(0u16.to_be_bytes())
            .peekable();

        let mut remining = CHAT_CAP - self.data.len();
        while iter.peek().is_some() {
            if remining == 0 {
                self.pop();
                remining = CHAT_CAP - self.data.len();
            }

            self.data
                .extend(iter.by_ref().take(remining).inspect(|_| remining -= 1));
        }

        let len = (self.data.len() - prev_len - 4) as u16;
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

        self.offset += len as u32 + 4;
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

    pub fn fetch(&self, mut cursor: Cursor, limit: usize, buffer: &mut Vec<u8>) -> Cursor {
        // cursor can be invalid so code does not assume anithing
        // complexity should only decrease if cursor is invalid
        cursor = cursor.min(self.offset);

        let to_skip = (self.offset - cursor) as usize;
        let mut iter = self.data.iter().rev();
        if iter.advance_by(to_skip).is_err() {
            return cursor;
        }

        for _ in 0..MESSAGE_FETCH_LIMIT.min(limit) {
            // we use le since we are reversed
            let Ok(header) = iter.by_ref().copied().next_chunk().map(u16::from_le_bytes) else {
                cursor = NO_CURSOR;
                break;
            };

            if header > MAX_MESSAGE_SIZE as u16 {
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
    use super::MailBlob;

    #[test]
    fn test_push() {
        let mut blob = super::MessageBlob::default();

        blob.push(b"hello".iter().cloned());
        blob.push(b"world".iter().cloned());

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
            blob.push([i, i + 1]);
        }

        let mut buffer = Vec::new();
        let mut cursor = super::NO_CURSOR;

        cursor = blob.fetch(cursor, 2, &mut buffer);

        assert_eq!(buffer, vec![0, 2, 9, 10, 0, 2, 8, 9]);
        assert_eq!(cursor, 48);

        buffer.clear();
        cursor = blob.fetch(cursor, 2, &mut buffer);

        assert_eq!(buffer, vec![0, 2, 7, 8, 0, 2, 6, 7]);
        assert_eq!(cursor, 36);
    }

    #[test]
    fn test_read_messages_is_sound() {
        let mut mb = MailBlob::default();
        mb.push([42; 3]);
        let slice = mb.read();
        assert_eq!(&slice[2..], &[42; 3]);
        let slice = mb.read();
        assert_eq!(slice, &[]);
    }
}
