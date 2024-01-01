use {
    super::Nonce,
    crate::{Identity, Proof, Topic},
    component_utils::{arrayvec::ArrayString, Codec, Reminder},
    std::convert::Infallible,
};

pub const NO_CURSOR: Cursor = Cursor::MAX;
pub const CHAT_NAME_CAP: usize = 32;

pub type Cursor = u32;

pub type ChatName = ArrayString<CHAT_NAME_CAP>;
pub type RawChatName = [u8; CHAT_NAME_CAP];

impl Topic for ChatName {
    type Event<'a> = ChatEvent<'a>;
    type Record = Infallible;
}

#[derive(Codec)]
pub enum ChatEvent<'a> {
    Message(Proof, Reminder<'a>),
}

#[derive(Codec)]
pub struct ChatChecksums {
    pub size: usize,
    pub user_count: usize,
    pub message_count: usize,
}

#[derive(Codec)]
pub enum ChatAction<'a> {
    AddUser(Identity),
    SendMessage(Reminder<'a>),
}

component_utils::gen_simple_error! {
    enum CreateChatError {
        AlreadyExists => "chat already exists",
    }

    enum AddUserError {
        ChatNotFound => "chat not found",
        InvalidProof => "invalid proof",
        AlreadyExists => "user already exists",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
    }

    enum SendMessageError {
        ChatNotFound => "chat not found",
        InvalidProof => "invalid proof",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
        MessageTooLarge => "message too large",
    }

    enum FetchMessagesError {
        ChatNotFound => "chat not found",
    }
}
