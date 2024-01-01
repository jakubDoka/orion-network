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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum CreateChatError {
    #[error("chat already exists")]
    AlreadyExists,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum AddUserError {
    #[error("chat not found")]
    ChatNotFound,
    #[error("invalid proof")]
    InvalidProof,
    #[error("user already exists")]
    AlreadyExists,
    #[error("you are not a member")]
    NotMember,
    #[error("invalid action, expected nonce higher then {0}")]
    InvalidAction(Nonce),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum SendMessageError {
    #[error("chat not found")]
    ChatNotFound,
    #[error("invalid proof")]
    InvalidProof,
    #[error("you are not a member")]
    NotMember,
    #[error("invalid action, expected nonce higher then {0}")]
    InvalidAction(Nonce),
    #[error("message too large")]
    MessageTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum FetchMessagesError {
    #[error("chat not found")]
    ChatNotFound,
}
