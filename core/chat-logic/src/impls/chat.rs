use {
    super::Nonce,
    crate::{BlockNumber, Identity, Proof, Topic},
    component_utils::{arrayvec::ArrayString, Codec, Reminder},
    std::convert::Infallible,
};

pub const CHAT_NAME_CAP: usize = 32;

#[derive(Clone, Copy, Codec)]
pub struct Message<'a> {
    pub identiy: Identity,
    pub nonce: Nonce,
    pub content: Reminder<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec)]
pub struct Cursor {
    pub block: BlockNumber,
    pub offset: usize,
}

impl Cursor {
    pub const INIT: Self = Self {
        block: u64::MAX,
        offset: 0,
    };
}

pub type ChatName = ArrayString<CHAT_NAME_CAP>;
pub type RawChatName = [u8; CHAT_NAME_CAP];

impl Topic for ChatName {
    type Event<'a> = ChatEvent<'a>;
    type Record = Infallible;
}

#[derive(Codec)]
pub enum ChatEvent<'a> {
    Message(Proof<ChatName>, Reminder<'a>),
}

#[derive(Codec)]
pub struct ChatChecksums {
    pub size: usize,
    pub user_count: usize,
    pub message_count: usize,
}

#[derive(Codec, Clone, Copy)]
pub enum ChatAction<'a> {
    AddUser(Identity),
    SendMessage(Reminder<'a>),
}

impl From<Identity> for ChatAction<'_> {
    fn from(identity: Identity) -> Self {
        Self::AddUser(identity)
    }
}

impl<'a> From<Reminder<'a>> for ChatAction<'a> {
    fn from(reminder: Reminder<'a>) -> Self {
        Self::SendMessage(reminder)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum CreateChatError {
    #[error("chat already exists")]
    AlreadyExists,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum ChatActionError {
    #[error("chat not found")]
    ChatNotFound,
    #[error("invalid proof")]
    InvalidProof,
    #[error("you are not a member")]
    NotMember,
    #[error("user already exists")]
    AlreadyMember,
    #[error("invalid action, expected nonce higher then {0}")]
    InvalidAction(Nonce),
    #[error("message too large")]
    MessageTooLarge,
    #[error("latest message block is still being finalized")]
    MessageBlockNotFinalized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum FetchMessagesError {
    #[error("chat not found")]
    ChatNotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum ProposeMsgBlockError {
    #[error("The sending node is not among replicators")]
    NoReplicator,
    #[error("chat not found")]
    ChatNotFound,
    #[error("no blocks even though past block was proposed")]
    NoBlocks,
    #[error("only server can propose blocks")]
    NotServer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum SendBlockError {
    #[error("not a replicator")]
    NoReplicator,
    #[error("chat not found")]
    ChatNotFound,
    #[error("invalid block: {0}")]
    InvalidBlock(InvalidBlockReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Codec, thiserror::Error)]
pub enum InvalidBlockReason {
    #[error("does not match majority")]
    MajorityMismatch,
    #[error("is uotdated for us")]
    Outdated,
    #[error("not expected at this point")]
    NotExpected,
}
