use crate::{advance_nonce, impls::storage::replicate, Proof};

use {
    crate::{ChatName, Identity, Nonce, Storage},
    component_utils::Reminder,
    std::collections::VecDeque,
};

const CHAT_CAP: usize = 1024 * 1024;
const MAX_MESSAGE_SIZE: usize = 1024;
const MESSAGE_FETCH_LIMIT: usize = 20;

pub const NO_CURSOR: Cursor = Cursor::MAX;

pub type Cursor = u32;

pub enum CreateChat {}

impl crate::SyncHandler for CreateChat {
    type Request<'a> = (Identity, ChatName);
    type Response<'a> = Result<(), CreateChatError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = ChatName;

    fn execute<'a>(
        context: &'a mut Self::Context,
        &(identiy, name): &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'a> {
        let chat_entry = context.store_mut().chats.entry(name);
        crate::ensure!(
            let std::collections::hash_map::Entry::Vacant(entry) = chat_entry,
            CreateChatError::AlreadyExists
        );

        entry.insert(Chat::new(identiy));
        replicate::<Self>(context, &name, &(identiy, name), meta);

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error CreateChatError {
        AlreadyExists => "chat already exists",
    }
}

pub enum AddUser {}

impl crate::SyncHandler for AddUser {
    type Request<'a> = (Identity, ChatName, Proof);
    type Response<'a> = Result<(), AddUserError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = ChatName;

    fn execute<'a>(
        context: &'a mut Self::Context,
        &(identiy, name, proof): &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'a> {
        ensure!(proof.verify_profile(), AddUserError::InvalidProof);

        let chat = context
            .store_mut()
            .chats
            .get_mut(&name)
            .ok_or(AddUserError::InvalidAction)?;

        let requester_id = crypto::hash::new_raw(&proof.pk);
        let requester = chat
            .members
            .iter_mut()
            .find(|m| m.id == requester_id)
            .ok_or(AddUserError::NotMember)?;

        ensure!(
            advance_nonce(&mut requester.action, proof.nonce),
            AddUserError::InvalidAction
        );

        ensure!(
            chat.members.iter().all(|m| m.id != identiy),
            AddUserError::AlreadyExists
        );

        chat.members.push(Member::new(identiy));
        replicate::<Self>(context, &name, &(identiy, name, proof), meta);

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error AddUserError {
        InvalidProof => "invalid proof",
        AlreadyExists => "user already exists",
        NotMember => "you are not a member",
        InvalidAction => "invalid action",
    }
}

pub enum SendMessage {}

impl crate::SyncHandler for SendMessage {
    type Request<'a> = (ChatName, Proof, Reminder<'a>);
    type Response<'a> = Result<(), SendMessageError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Event<'a> = (Proof, Reminder<'a>);
    type Topic = ChatName;

    fn execute<'a>(
        context: &'a mut Self::Context,
        &(name, proof, message): &Self::Request<'a>,
        events: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'a> {
        ensure!(proof.verify_chat(name), SendMessageError::InvalidProof);

        ensure!(
            message.0.len() <= MAX_MESSAGE_SIZE,
            SendMessageError::MessageTooLarge
        );

        let chat = context
            .store_mut()
            .chats
            .get_mut(&name)
            .ok_or(SendMessageError::ChatNotFound)?;

        let sender_id = crypto::hash::new_raw(&proof.pk);
        let sender = chat
            .members
            .iter_mut()
            .find(|m| m.id == sender_id)
            .ok_or(SendMessageError::NotMember)?;

        ensure!(
            advance_nonce(&mut sender.action, proof.nonce),
            SendMessageError::InvalidAction
        );

        chat.messages.push(message.0.iter().copied());
        replicate::<Self>(context, &name, &(name, proof, message), meta);
        events.push(name, &(proof, message));

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error SendMessageError {
        InvalidProof => "invalid proof",
        ChatNotFound => "chat not found",
        NotMember => "you are not a member",
        InvalidAction => "invalid action",
        MessageTooLarge => "message too large",
    }
}

pub enum FetchMessages {}

impl crate::SyncHandler for FetchMessages {
    type Request<'a> = (ChatName, Cursor);
    type Response<'a> = Result<(Vec<u8>, Cursor), FetchMessagesError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = ChatName;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Self::Response<'a> {
        let chat = context
            .store_mut()
            .chats
            .get_mut(&request.0)
            .ok_or(FetchMessagesError::ChatNotFound)?;

        let mut buffer = Vec::new();
        let cursor = chat
            .messages
            .fetch(request.1, MESSAGE_FETCH_LIMIT, &mut buffer);

        Ok((buffer, cursor))
    }
}

component_utils::gen_simple_error! {
    error FetchMessagesError {
        ChatNotFound => "chat not found",
    }
}

component_utils::protocol! {'a:
    struct Chat {
        members: Vec<Member>,
        messages: MessageBlob,
    }

    struct Member {
        id: Identity,
        action: Nonce,
    }

    #[derive(Clone, Copy)]
    struct ChatHistory<'a> {
        offset: Cursor,
        first: &'a [u8],
        last: Reminder<'a>,
    }


    #[derive(Default)]
    struct MessageBlob {
        data: VecDeque<u8>,
        offset: Cursor,
    }

    #[derive(Clone, Copy)]
    struct Message<'a> {
        identiy: Identity,
        content: Reminder<'a>,
    }
}

impl Chat {
    pub fn new(id: Identity) -> Self {
        Self {
            members: vec![Member::new(id)],
            messages: MessageBlob::default(),
        }
    }
}

impl Member {
    pub fn new(id: Identity) -> Self {
        Self { id, action: 0 }
    }
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
        if c.offset < self.offset || c.first.len() + c.last.0.len() <= self.data.len() {
            return;
        }

        self.data.clear();
        self.data.extend(c.first);
        self.data.extend(c.last.0);
        self.offset = c.offset;
    }
}

#[cfg(test)]
mod test {
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
}
