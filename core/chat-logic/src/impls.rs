use {
    crate::Protocol,
    component_utils::{Codec, Reminder},
    crypto::{enc, sign, Serialized, TransmutationCircle},
    rand_core::OsRng,
    std::{convert::Infallible, fmt::Debug, iter, num::NonZeroUsize},
};
pub use {chat::*, profile::*};

pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(4) };

pub type Nonce = u64;
pub type BlockNumber = usize;
pub type ProofContext = [u8; CHAT_NAME_CAP];
pub type Identity = crypto::Hash;

mod chat;
mod profile;

macro_rules! compose_protocols {
    ($(
        fn $for:ident<$lt:lifetime>($($req:ty),*) -> Result<$resp:ty, $error:ty>;
    )*) => {$(
        pub enum $for {}
        impl $crate::extractors::Protocol for $for {
            const PREFIX: u8 = ${index(0)};
            type Error = $error;
            #[allow(unused_parens)]
            type Request<$lt> = ($($req),*);
            type Response<$lt> = $resp;
        }
    )*};
}

compose_protocols! {
    fn Subscribe<'a>(PossibleTopic) -> Result<(), Infallible>;

    fn CreateChat<'a>(ChatName, Identity) -> Result<(), CreateChatError>;
    fn PerformChatAction<'a>(ChatName, Proof, ChatAction<'a>) -> Result<(), ChatActionError>;
    fn FetchMessages<'a>(ChatName, Cursor) -> Result<(Cursor, Reminder<'a>), FetchMessagesError>;
    fn ProposeMsgBlock<'a>(ChatName, BlockNumber, crypto::Hash) -> Result<(), ProposeMsgBlockError>;
    fn SendBlock<'a>(ChatName, BlockNumber, Reminder<'a>) -> Result<(), SendBlockError>;

    fn CreateProfile<'a>(Proof, Serialized<enc::PublicKey>, Reminder<'a>) -> Result<(), CreateAccountError>;
    fn SetVault<'a>(Proof, Reminder<'a>) -> Result<(), SetVaultError>;
    fn FetchVault<'a>(Identity) -> Result<(Nonce, Nonce, Reminder<'a>), FetchVaultError>;
    fn ReadMail<'a>(Proof) -> Result<Reminder<'a>, ReadMailError>;
    fn SendMail<'a>(Identity, Reminder<'a>) -> Result<(), SendMailError>;
    fn FetchProfile<'a>(Identity) -> Result<FetchProfileResp, FetchProfileError>;
    fn FetchFullProfile<'a>(Identity) -> Result<BorrowedProfile<'a>, FetchProfileError>;
}

pub struct Repl<T: Protocol>(T);

impl<T: Protocol> Protocol for Repl<T> {
    type Error = ReplError<T::Error>;
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;

    const PREFIX: u8 = T::PREFIX;
}

#[derive(Debug, PartialEq, Eq, thiserror::Error, Codec)]
pub enum ReplError<T> {
    #[error("no majority")]
    NoMajority,
    #[error("invalid response from majority")]
    InvalidResponse,
    #[error("invalid topic")]
    InvalidTopic,
    #[error(transparent)]
    Inner(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Codec)]
pub enum PossibleTopic {
    Profile(Identity),
    Chat(ChatName),
}

impl PossibleTopic {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Profile(i) => i.as_ref(),
            Self::Chat(c) => c.as_bytes(),
        }
    }
}

pub trait ToPossibleTopic {
    fn to_possible_topic(&self) -> PossibleTopic;
}

impl ToPossibleTopic for Identity {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Profile(*self)
    }
}

impl ToPossibleTopic for Proof {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Profile(crypto::hash::from_raw(&self.pk))
    }
}

impl ToPossibleTopic for ChatName {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Chat(*self)
    }
}

impl<A, B> ToPossibleTopic for (A, B)
where
    A: ToPossibleTopic,
{
    fn to_possible_topic(&self) -> PossibleTopic {
        self.0.to_possible_topic()
    }
}

impl<A, B, C> ToPossibleTopic for (A, B, C)
where
    A: ToPossibleTopic,
{
    fn to_possible_topic(&self) -> PossibleTopic {
        self.0.to_possible_topic()
    }
}

#[test]
fn dec_end_untagged_enum() {
    fn enc_dec(t: PossibleTopic) {
        let mut buf = Vec::new();
        t.encode(&mut buf).unwrap();
        assert_eq!(PossibleTopic::decode(&mut buf.as_slice()), Some(t));
    }

    enc_dec(PossibleTopic::Chat(ChatName::from("test").unwrap()));
    enc_dec(PossibleTopic::Profile(Identity::default()));
}

impl From<ChatName> for PossibleTopic {
    fn from(c: ChatName) -> Self {
        Self::Chat(c)
    }
}

impl From<Identity> for PossibleTopic {
    fn from(i: Identity) -> Self {
        Self::Profile(i)
    }
}

pub fn advance_nonce(current: &mut Nonce, new: Nonce) -> bool {
    let valid = new > *current;
    if valid {
        *current = new;
    }
    valid
}

pub fn unpack_messages(mut buffer: &mut [u8]) -> impl Iterator<Item = &mut [u8]> {
    iter::from_fn(move || {
        let len = buffer.take_mut(buffer.len().wrapping_sub(2)..)?;
        let len = u16::from_be_bytes(len.try_into().unwrap());
        buffer.take_mut(buffer.len().wrapping_sub(len as usize)..)
    })
}

pub fn unpack_mail(mut buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    iter::from_fn(move || {
        let len = buffer.take(..2)?;
        let len = u16::from_be_bytes(len.try_into().unwrap());
        buffer.take(..len as usize)
    })
}

#[derive(Clone, Copy, Codec)]
pub struct Proof {
    pub pk: Serialized<sign::PublicKey>,
    pub nonce: Nonce,
    pub signature: Serialized<sign::Signature>,
}

impl Proof {
    const PAYLOAD_SIZE: usize = std::mem::size_of::<Nonce>() + CHAT_NAME_CAP;

    pub fn for_mail(kp: &sign::Keypair, nonce: &mut Nonce) -> Self {
        Self::new(kp, nonce, [0xff - 1; CHAT_NAME_CAP])
    }

    pub fn for_vault(kp: &sign::Keypair, nonce: &mut Nonce, vault: &[u8]) -> Self {
        Self::new(kp, nonce, crypto::hash::from_slice(vault))
    }

    pub fn for_chat(kp: &sign::Keypair, nonce: &mut Nonce, chat_name: ChatName) -> Self {
        Self::new(kp, nonce, component_utils::arrstr_to_array(chat_name))
    }

    fn new(kp: &sign::Keypair, nonce: &mut Nonce, context: ProofContext) -> Self {
        let signature = kp.sign(&Self::pack_payload(*nonce, context), OsRng);
        *nonce += 1;
        Self {
            pk: kp.public_key().into_bytes(),
            nonce: *nonce - 1,
            signature: signature.into_bytes(),
        }
    }

    fn pack_payload(nonce: Nonce, context: ProofContext) -> [u8; Self::PAYLOAD_SIZE] {
        let mut buf = [0; Self::PAYLOAD_SIZE];
        buf[..CHAT_NAME_CAP].copy_from_slice(&context);
        buf[CHAT_NAME_CAP..].copy_from_slice(&nonce.to_be_bytes());
        buf
    }

    pub fn verify_mail(&self) -> bool {
        self.verify([0xff - 1; CHAT_NAME_CAP])
    }

    pub fn verify_vault(&self, vault: &[u8]) -> bool {
        self.verify(crypto::hash::from_slice(vault))
    }

    pub fn verify_chat(&self, chat_name: ChatName) -> bool {
        self.verify(component_utils::arrstr_to_array(chat_name))
    }

    fn verify(&self, context: ProofContext) -> bool {
        let bytes = Self::pack_payload(self.nonce, context);
        let pk = sign::PublicKey::from_ref(&self.pk);
        let signature = sign::Signature::from_ref(&self.signature);
        pk.verify(&bytes, signature).is_ok()
    }
}
