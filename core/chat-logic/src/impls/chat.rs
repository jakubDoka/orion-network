use {
    super::{Identity, Nonce},
    component_utils::Reminder,
    std::collections::VecDeque,
};

const CHAT_CAP: usize = 1024 * 1024;
const MAX_MESSAGE_SIZE: usize = 1024;
const MESSAGE_FETCH_LIMIT: usize = 20;

pub const NO_CURSOR: Cursor = Cursor::MAX;

pub type Cursor = u32;

component_utils::gen_simple_error! {
    error CreateChatError {
        AlreadyExists => "chat already exists",
    }
}

component_utils::gen_simple_error! {
    error AddUserError {
        InvalidProof => "invalid proof",
        AlreadyExists => "user already exists",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
        ChatNotFound => "chat not found",
    }
}

component_utils::gen_simple_error! {
    error SendMessageError {
        InvalidProof => "invalid proof",
        ChatNotFound => "chat not found",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
        MessageTooLarge => "message too large",
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
