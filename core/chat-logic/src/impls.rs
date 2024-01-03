use {
    crate::Protocol,
    component_utils::{Codec, Reminder},
    crypto::{enc, sign, Serialized, TransmutationCircle},
    rand_core::OsRng,
    std::{convert::Infallible, fmt::Debug, iter, num::NonZeroUsize, ops::Range, usize},
};
pub use {chat::*, profile::*};

pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(4) };

pub type Nonce = u64;
pub type BlockNumber = u64;
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
    fn PerformChatAction<'a>(Proof<ChatName>, ChatAction<'a>) -> Result<(), ChatActionError>;
    fn FetchMessages<'a>(ChatName, Cursor) -> Result<(Cursor, Reminder<'a>), FetchMessagesError>;
    fn ProposeMsgBlock<'a>(ChatName, BlockNumber, crypto::Hash) -> Result<(), ProposeMsgBlockError>;
    fn SendBlock<'a>(ChatName, BlockNumber, Reminder<'a>) -> Result<(), SendBlockError>;

    fn CreateProfile<'a>(Proof<&'a [u8]>, Serialized<enc::PublicKey>) -> Result<(), CreateAccountError>;
    fn SetVault<'a>(Proof<Reminder<'a>>) -> Result<(), SetVaultError>;
    fn FetchVault<'a>(Identity) -> Result<(Nonce, Nonce, Reminder<'a>), FetchVaultError>;
    fn ReadMail<'a>(Proof<Mail>) -> Result<Reminder<'a>, ReadMailError>;
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

impl<'a> ToPossibleTopic for Proof<&'a [u8]> {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Profile(crypto::hash::from_raw(&self.pk))
    }
}

impl<'a> ToPossibleTopic for Proof<Reminder<'a>> {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Profile(crypto::hash::from_raw(&self.pk))
    }
}

impl ToPossibleTopic for Proof<ChatName> {
    fn to_possible_topic(&self) -> PossibleTopic {
        PossibleTopic::Chat(self.context)
    }
}

impl ToPossibleTopic for Proof<Mail> {
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

pub fn unpack_messages_ref(mut buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    iter::from_fn(move || {
        let len = buffer.take(buffer.len().wrapping_sub(2)..)?;
        let len = u16::from_be_bytes(len.try_into().unwrap());
        buffer.take(buffer.len().wrapping_sub(len as usize)..)
    })
}

pub fn unpack_mail(mut buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    iter::from_fn(move || {
        let len = buffer.take(..2)?;
        let len = u16::from_be_bytes(len.try_into().unwrap());
        buffer.take(..len as usize)
    })
}

pub fn retain_messages_in_vec(buffer: &mut Vec<u8>, predicate: impl FnMut(&mut [u8]) -> bool) {
    let len = retain_messages(buffer, predicate).len();
    buffer.drain(..buffer.len() - len);
}

/// moves all kept messages to the end of the slice and returns the kept region,
/// data at the begginig of the `buffer` is arbitrary, invalid codec is stripped
pub fn retain_messages(
    buffer: &mut [u8],
    mut predicate: impl FnMut(&mut [u8]) -> bool,
) -> &mut [u8] {
    fn move_mem(hole_end: *mut u8, cursor: *mut u8, write_cursor: &mut *mut u8, len: usize) {
        if hole_end == cursor {
            return;
        }

        let write_len = hole_end as usize - cursor as usize - len;
        if hole_end != *write_cursor {
            unsafe {
                std::ptr::copy(
                    hole_end.sub(write_len),
                    write_cursor.sub(write_len),
                    write_len,
                )
            };
        }

        *write_cursor = unsafe { write_cursor.sub(write_len) };
    }

    let Range { start, end } = buffer.as_mut_ptr_range();
    let [mut write_cursor, mut cursor, mut hole_end] = [end; 3];

    loop {
        if (cursor as usize - start as usize) < 2 {
            break;
        }

        let len = unsafe { u16::from_be_bytes(*cursor.sub(2).cast::<[u8; 2]>()) };
        let len = len as usize;

        if (cursor as usize - start as usize) < len + 2 {
            break;
        }

        cursor = unsafe { cursor.sub(len + 2) };
        let slice = unsafe { std::slice::from_raw_parts_mut(cursor, len) };
        if predicate(slice) {
            continue;
        }

        move_mem(hole_end, cursor, &mut write_cursor, len + 2);

        hole_end = cursor;
    }

    move_mem(hole_end, cursor, &mut write_cursor, 0);

    unsafe { std::slice::from_mut_ptr_range(write_cursor..end) }
}

#[derive(Clone, Copy, Codec)]
pub struct Proof<T> {
    pub pk: Serialized<sign::PublicKey>,
    pub nonce: Nonce,
    pub signature: Serialized<sign::Signature>,
    pub context: T,
}

const PAYLOAD_SIZE: usize = std::mem::size_of::<Nonce>() + CHAT_NAME_CAP;

impl<T: ToProofContext> Proof<T> {
    pub fn new(kp: &sign::Keypair, nonce: &mut Nonce, context: T) -> Self {
        let signature = kp.sign(
            &Self::pack_payload(*nonce, context.to_proof_context()),
            OsRng,
        );
        *nonce += 1;
        Self {
            pk: kp.public_key().into_bytes(),
            nonce: *nonce - 1,
            signature: signature.into_bytes(),
            context,
        }
    }

    fn pack_payload(nonce: Nonce, context: ProofContext) -> [u8; PAYLOAD_SIZE] {
        let mut buf = [0; PAYLOAD_SIZE];
        buf[..CHAT_NAME_CAP].copy_from_slice(&context);
        buf[CHAT_NAME_CAP..].copy_from_slice(&nonce.to_be_bytes());
        buf
    }

    pub fn verify(&self) -> bool {
        let bytes = Self::pack_payload(self.nonce, self.context.to_proof_context());
        let pk = sign::PublicKey::from_ref(&self.pk);
        let signature = sign::Signature::from_ref(&self.signature);
        pk.verify(&bytes, signature).is_ok()
    }
}

pub trait ToProofContext: Copy {
    fn to_proof_context(self) -> ProofContext;
}

impl ToProofContext for ChatName {
    fn to_proof_context(self) -> ProofContext {
        component_utils::arrstr_to_array(self)
    }
}

#[derive(Clone, Copy, Codec)]
pub struct Mail;

impl ToProofContext for Mail {
    fn to_proof_context(self) -> ProofContext {
        [0xff - 1; CHAT_NAME_CAP]
    }
}

impl<'a> ToProofContext for Reminder<'a> {
    fn to_proof_context(self) -> ProofContext {
        crypto::hash::from_slice(self.0)
    }
}

impl<'a> ToProofContext for &'a [u8] {
    fn to_proof_context(self) -> ProofContext {
        crypto::hash::from_slice(self)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn retain_messages() {
        let input = [
            &[],
            &[0][..],
            &[0, 0],
            &[0, 0, 0, 0],
            &[0, 1, 0, 2, 0, 0],
            &[0, 0, 1, 0, 0, 4, 0, 0, 0, 2],
            &[0, 0, 1, 0, 0, 4, 0, 0, 0, 2, 1, 0, 1],
            &[1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1],
            &[1, 0, 0],
            &[0, 0, 0, 3],
            &[0, 0, 20],
        ];

        let output = [
            &[][..],
            &[],
            &[0, 0],
            &[0, 0, 0, 0],
            &[0, 0],
            &[0, 0, 0, 2],
            &[0, 0, 0, 2],
            &[0, 0],
            &[0, 0],
            &[],
            &[],
        ];

        for (input, output) in input.iter().zip(output.iter()) {
            let mut owned_intput = input.to_vec();
            let output = output.to_vec();
            let real_out =
                crate::retain_messages(&mut owned_intput, |bts| bts.iter().all(|b| *b == 0));
            assert_eq!(real_out, output.as_slice(), "input: {:?}", input);
        }
    }
}
