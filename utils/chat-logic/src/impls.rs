use std::{iter, num::NonZeroUsize};

use component_utils::Reminder;

use crate::RequestId;

use {
    component_utils::arrayvec::ArrayString,
    crypto::{sign, Serialized, TransmutationCircle},
};

pub const CHAT_NAME_CAP: usize = 32;
pub const QUORUM: libp2p::kad::Quorum = libp2p::kad::Quorum::Majority;
pub const REPLICATION_FACTOR: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(4) };

pub type ChatName = ArrayString<CHAT_NAME_CAP>;
pub type Nonce = u64;
pub type ProofContext = [u8; CHAT_NAME_CAP];
pub type Identity = crypto::Hash<sign::PublicKey>;

pub fn advance_nonce(current: &mut Nonce, new: Nonce) -> bool {
    if new > *current {
        *current = new;
        true
    } else {
        false
    }
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

        let (slice, rest) = std::mem::take(&mut iter)
            .into_slice()
            .split_at_mut(len as usize);
        iter = rest.iter_mut();

        Some(slice)
    })
}

mod search_peers;
mod storage;

pub use {search_peers::*, storage::*};

compose_handlers! {
    Server {
        sp: SearchPeers<ProfileQ>,
        sc: SearchPeers<ChatQ>,

        ca: CreateAccount,
        sv: SetVault,
        fv: FetchVault,
        sm: SendMail,
        rm: ReadMail,
        fp: FetchProfile,

        cp: CreateChat,
        au: AddUser,
        smsg: SendMessage,
        fm: FetchMessages,
    }
}

component_utils::protocol! {'a:
    #[derive(Clone, Copy)]
    struct Proof {
        pk: Serialized<sign::PublicKey>,
        nonce: Nonce,
        signature: Serialized<sign::Signature>,
    }

    #[derive(Clone, Copy)]
    struct DispatchResponse<'a> {
        id: RequestId,
        body: Reminder<'a>,
    }
}

impl Proof {
    const PROFILE_CONTEXT: ProofContext = [0xff - 1; CHAT_NAME_CAP];

    pub fn for_profile(kp: &sign::KeyPair, nonce: &mut Nonce) -> Self {
        Self::new(kp, nonce, Self::PROFILE_CONTEXT)
    }

    pub fn for_chat(kp: &sign::KeyPair, nonce: &mut Nonce, chat_name: ChatName) -> Self {
        Self::new(kp, nonce, component_utils::arrstr_to_array(chat_name))
    }

    fn new(kp: &sign::KeyPair, nonce: &mut Nonce, context: ProofContext) -> Self {
        let signature = kp.sign(&Self::pack_proof_and_nonce(*nonce, context));
        *nonce += 1;
        Self {
            pk: kp.public_key().into_bytes(),
            nonce: *nonce - 1,
            signature: signature.into_bytes(),
        }
    }

    fn pack_proof_and_nonce(nonce: Nonce, context: ProofContext) -> [u8; 36] {
        let mut buf = [0; 36];
        buf[..32].copy_from_slice(&context);
        buf[32..].copy_from_slice(&nonce.to_be_bytes());
        buf
    }

    pub fn verify_profile(&self) -> bool {
        self.verify(Self::PROFILE_CONTEXT)
    }

    pub fn verify_chat(&self, chat_name: ChatName) -> bool {
        self.verify(component_utils::arrstr_to_array(chat_name))
    }

    fn verify(&self, context: ProofContext) -> bool {
        let bytes = Self::pack_proof_and_nonce(self.nonce, context);
        let pk = sign::PublicKey::from_ref(&self.pk);
        let signature = sign::Signature::from_ref(&self.signature);
        pk.verify(&bytes, signature).is_ok()
    }
}
