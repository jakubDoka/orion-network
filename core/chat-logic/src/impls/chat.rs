use {
    super::Nonce,
    crate::{Proof, Topic},
    component_utils::{arrayvec::ArrayString, Reminder},
};

pub const NO_CURSOR: Cursor = Cursor::MAX;
pub const CHAT_NAME_CAP: usize = 32;

pub type Cursor = u32;

pub type ChatName = ArrayString<CHAT_NAME_CAP>;
pub type RawChatName = [u8; CHAT_NAME_CAP];

impl Topic for ChatName {
    type Event<'a> = ChatEvent<'a>;
}

component_utils::protocol! { 'a:
    enum ChatEvent<'a> {
        Message: (Proof, Reminder<'a>),
    }
}

component_utils::gen_simple_error! {
    error CreateChatError {
        AlreadyExists => "chat already exists",
    }

    error AddUserError {
        InvalidProof => "invalid proof",
        AlreadyExists => "user already exists",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
        ChatNotFound => "chat not found",
    }

    error SendMessageError {
        InvalidProof => "invalid proof",
        ChatNotFound => "chat not found",
        NotMember => "you are not a member",
        InvalidAction(Nonce) => "invalid action, expected nonce higher then {0}",
        MessageTooLarge => "message too large",
    }

    error FetchMessagesError {
        ChatNotFound => "chat not found",
    }
}
