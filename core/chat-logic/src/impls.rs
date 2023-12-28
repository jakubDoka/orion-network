use {
    crate::{ExtractTopic, Protocol},
    component_utils::{codec, Codec, Reminder},
    crypto::{enc, sign, Serialized, TransmutationCircle},
    std::{borrow::Borrow, convert::Infallible, fmt::Debug, iter, num::NonZeroUsize},
};
pub use {chat::*, profile::*};

pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(4) };

pub type Nonce = u64;
pub type ProofContext = [u8; CHAT_NAME_CAP];
pub type Identity = crypto::Hash<sign::PublicKey>;

mod chat;
mod profile;

crate::compose_protocols! {
    fn Subscribe<'a>(PossibleTopic) -> Result<(), Infallible>;

    fn CreateChat<'a>(Identity, ChatName) -> Result<(), CreateChatError>
        where Topic(ChatName): |&(.., c)| c;
    fn AddUser<'a>(Identity, ChatName, Proof) -> Result<(), AddUserError>
        where Topic(ChatName): |&(_, c, ..)| c;
    fn SendMessage<'a>(ChatName, Proof, Reminder<'a>) -> Result<(), SendMessageError>
        where Topic(ChatName): |&(c, ..)| c;
    fn FetchMessages<'a>(ChatName, Cursor) -> Result<(Vec<u8>, Cursor), FetchMessagesError>
        where Topic(ChatName): |&(c, ..)| c;

    fn CreateProfile<'a>(Proof, Serialized<enc::PublicKey>, Reminder<'a>) -> Result<(), CreateAccountError>
        where Topic(Identity): |(p, ..)| crypto::hash::new_raw(&p.pk);
    fn SetVault<'a>(Proof, Reminder<'a>) -> Result<(), SetVaultError>
        where Topic(Identity): |(p, ..)| crypto::hash::new_raw(&p.pk);
    fn FetchVault<'a>(Identity) -> Result<(Nonce, Nonce, Reminder<'a>), FetchVaultError>
        where Topic(Identity): |&i| i;
    fn ReadMail<'a>(Proof) -> Result<Reminder<'a>, ReadMailError>
        where Topic(Identity): |p| crypto::hash::new_raw(&p.pk);
    fn SendMail<'a>(Identity, Reminder<'a>) -> Result<(), SendMailError>
        where Topic(Identity): |&(i, ..)| i;
    fn FetchProfile<'a>(Identity) -> Result<FetchProfileResp, FetchProfileError>
        where Topic(Identity): |&i| i;
}

pub struct Repl<T: Protocol>(T);

impl<T: Protocol> Protocol for Repl<T> {
    type Error = ReplError<T::Error>;
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;

    const PREFIX: u8 = T::PREFIX;
}

impl<T: ExtractTopic> ExtractTopic for Repl<T> {
    type Topic = T::Topic;

    fn extract_topic(request: &Self::Request<'_>) -> Self::Topic {
        T::extract_topic(request)
    }
}

#[derive(Debug, thiserror::Error)]
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

impl<'a, T: Codec<'a>> Codec<'a> for ReplError<T> {
    fn encode(&self, buf: &mut impl codec::Buffer) -> Option<()> {
        match self {
            Self::NoMajority => buf.push(0),
            Self::Inner(e) => {
                buf.push(1)?;
                e.encode(buf)
            }
            Self::InvalidResponse => buf.push(2),
            Self::InvalidTopic => buf.push(3),
        }
    }

    fn decode(buf: &mut &'a [u8]) -> Option<Self> {
        match buf.take_first()? {
            0 => Some(Self::NoMajority),
            1 => Some(Self::Inner(T::decode(buf)?)),
            2 => Some(Self::InvalidResponse),
            3 => Some(Self::InvalidTopic),
            _ => None,
        }
    }
}

component_utils::protocol! {'a:
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    untagged_enum PossibleTopic {
        Profile: Identity,
        Chat: ChatName,
    }
}

impl Borrow<[u8]> for PossibleTopic {
    fn borrow(&self) -> &[u8] {
        match self {
            Self::Profile(i) => i.0.as_slice(),
            Self::Chat(c) => c.as_bytes(),
        }
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

pub fn unpack_messages(buffer: &mut [u8]) -> impl Iterator<Item = &mut [u8]> {
    let mut iter = buffer.iter_mut();
    iter::from_fn(move || {
        let len = iter
            .by_ref()
            .map(|b| *b)
            .next_chunk()
            .map(u16::from_be_bytes)
            .ok()?;

        if len > iter.as_slice().len() as u16 {
            return None;
        }

        let (slice, rest) = std::mem::take(&mut iter)
            .into_slice()
            .split_at_mut(len as usize);
        iter = rest.iter_mut();

        Some(slice)
    })
}

pub fn unpack_messages_ref(buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut iter = buffer.iter();
    iter::from_fn(move || {
        let len = iter
            .by_ref()
            .copied()
            .next_chunk()
            .map(u16::from_be_bytes)
            .ok()?;

        if len > iter.as_slice().len() as u16 {
            return None;
        }

        let (slice, rest) = iter.as_slice().split_at(len as usize);
        iter = rest.iter();

        Some(slice)
    })
}

component_utils::protocol! {'a:
    #[derive(Clone, Copy)]
    struct Proof {
        pk: Serialized<sign::PublicKey>,
        nonce: Nonce,
        signature: Serialized<sign::Signature>,
    }
}

impl Proof {
    const PAYLOAD_SIZE: usize = std::mem::size_of::<Nonce>() + CHAT_NAME_CAP;

    pub fn for_mail(kp: &sign::KeyPair, nonce: &mut Nonce) -> Self {
        Self::new(kp, nonce, [0xff - 1; CHAT_NAME_CAP])
    }

    pub fn for_vault(kp: &sign::KeyPair, nonce: &mut Nonce, vault: &[u8]) -> Self {
        Self::new(kp, nonce, crypto::hash::new_slice(vault).0)
    }

    pub fn for_chat(kp: &sign::KeyPair, nonce: &mut Nonce, chat_name: ChatName) -> Self {
        Self::new(kp, nonce, component_utils::arrstr_to_array(chat_name))
    }

    fn new(kp: &sign::KeyPair, nonce: &mut Nonce, context: ProofContext) -> Self {
        let signature = kp.sign(&Self::pack_payload(*nonce, context));
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
        self.verify(crypto::hash::new_slice(vault).0)
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
