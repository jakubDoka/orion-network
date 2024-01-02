use {super::*, chat_logic::*, component_utils::Reminder, std::collections::VecDeque};

const CHAT_CAP: usize = 1024 * 1024;
const MAX_MESSAGE_SIZE: usize = 1024;
const MESSAGE_FETCH_LIMIT: usize = 20;

impl SyncHandler for CreateChat {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (identity, name): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        let chat_entry = cx.storage.chats.entry(name);
        crate::ensure!(
            let std::collections::hash_map::Entry::Vacant(entry) = chat_entry,
            CreateChatError::AlreadyExists
        );

        entry.insert(Chat::new(identity));

        Ok(())
    }
}

impl SyncHandler for AddUser {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (identiy, name, proof): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        ensure!(proof.verify_chat(name), AddUserError::InvalidProof);

        let chat = cx
            .storage
            .chats
            .get_mut(&name)
            .ok_or(AddUserError::ChatNotFound)?;

        let requester_id = crypto::hash::from_raw(&proof.pk);
        let requester = chat
            .members
            .iter_mut()
            .find(|m| m.id == requester_id)
            .ok_or(AddUserError::NotMember)?;

        ensure!(
            advance_nonce(&mut requester.action, proof.nonce),
            AddUserError::InvalidAction(requester.action)
        );

        ensure!(
            chat.members.iter().all(|m| m.id != identiy),
            AddUserError::AlreadyExists
        );

        chat.members.push(Member::new(identiy));

        Ok(())
    }
}

impl SyncHandler for SendMessage {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (name, proof, message): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        ensure!(proof.verify_chat(name), SendMessageError::InvalidProof);

        ensure!(
            message.0.len() <= MAX_MESSAGE_SIZE,
            SendMessageError::MessageTooLarge
        );

        let chat = cx
            .storage
            .chats
            .get_mut(&name)
            .ok_or(SendMessageError::ChatNotFound)?;

        let sender_id = crypto::hash::from_raw(&proof.pk);
        let sender = chat
            .members
            .iter_mut()
            .find(|m| m.id == sender_id)
            .ok_or(SendMessageError::NotMember)?;

        ensure!(
            advance_nonce(&mut sender.action, proof.nonce),
            SendMessageError::InvalidAction(sender.action)
        );

        chat.messages.push(message.0.iter().copied());
        cx.push(name, ChatEvent::Message(proof, message));

        Ok(())
    }
}

impl SyncHandler for FetchMessages {
    fn execute<'a>(mut cx: Scope<'a>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        let chat = cx
            .storage
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

#[derive(Codec)]
pub struct Chat {
    pub members: Vec<Member>,
    pub messages: MessageBlob,
}

#[derive(Codec)]
pub struct Member {
    pub id: Identity,
    pub action: Nonce,
}

#[derive(Clone, Copy, Codec)]
pub struct ChatHistory<'a> {
    pub offset: Cursor,
    pub first: &'a [u8],
    pub last: Reminder<'a>,
}

#[derive(Default, Codec)]
pub struct MessageBlob {
    pub data: VecDeque<u8>,
    pub offset: Cursor,
}

#[derive(Clone, Copy, Codec)]
pub struct Message<'a> {
    pub identiy: Identity,
    pub content: Reminder<'a>,
}

bitflags::bitflags! {
    pub struct Permissions: u8 {
        const MODIFY_PERMISSIONS = 1 << 0;
        const KICK = 1 << 1;
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
}

#[cfg(test)]
mod test {
    #[test]
    fn test_push() {
        let mut blob = super::MessageBlob::default();

        blob.push(b"hello".iter().cloned());
        blob.push(b"world".iter().cloned());

        assert_eq!(blob.data, vec![
            0, 5, b'h', b'e', b'l', b'l', b'o', 0, 5, 0, 5, b'w', b'o', b'r', b'l', b'd', 0, 5
        ]);

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
