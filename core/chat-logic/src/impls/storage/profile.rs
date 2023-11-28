use std::convert::Infallible;

use crate::advance_nonce;

const MAIL_BOX_CAP: usize = 1024 * 1024;

use {
    super::{replicate, Storage},
    crate::{Identity, Nonce, Proof},
    component_utils::{Codec, Reminder},
    crypto::{enc, sign, Serialized},
    libp2p::kad::{GetRecordOk, PeerRecord, QueryId},
    std::collections::hash_map::Entry,
};

component_utils::protocol! {'a:
    #[derive(Clone)]
    struct FullProfile {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        action: Nonce,
        vault: Vec<u8>,
        mail: Vec<u8>,
    }
}

impl FullProfile {
    fn read_mail(&mut self) -> &[u8] {
        let slice = unsafe { std::mem::transmute(self.mail.as_slice()) };
        unsafe { self.mail.set_len(0) };
        slice
    }
}

pub struct FetchProfile {
    id: QueryId,
}

impl crate::Handler for FetchProfile {
    type Request<'a> = Identity;
    type Response<'a> = Result<FetchProfileResp, FetchProfileError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = Infallible;

    fn spawn(
        context: &mut Self::Context,
        request: &Self::Request<'_>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Result<Self::Response<'static>, Self> {
        if let Some(profile) = context.store_mut().profiles.get(&request) {
            return Ok(Ok(profile.into()));
        }

        Err(Self {
            id: context.get_record(request.0.to_vec().to_vec().into()),
        })
    }

    fn try_complete(
        self,
        _context: &mut Self::Context,
        _: &mut crate::EventDispatch<Self>,
        event: &<Self::Context as crate::Context>::ToSwarm,
    ) -> Result<Self::Response<'static>, Self> {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: libp2p::kad::QueryResult::GetRecord(result),
            ..
        } = event
        else {
            return Err(self);
        };

        crate::ensure!(self.id == *id, self);

        let Ok(GetRecordOk::FoundRecord(PeerRecord { record, .. })) = result else {
            return Ok(Err(FetchProfileError::NotFound));
        };

        let res = FetchProfileResp::decode(&mut record.value.as_slice())
            .ok_or(FetchProfileError::InvalidRecord);
        Ok(res)
    }
}

component_utils::gen_simple_error! {
    error FetchProfileError {
        NotFound => "account not found",
        InvalidRecord => "invalid record",
    }
}

component_utils::protocol! {'a:
    struct FetchProfileResp {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
    }
}

impl From<&FullProfile> for FetchProfileResp {
    fn from(profile: &FullProfile) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
        }
    }
}

pub enum CreateAccount {}

impl crate::SyncHandler for CreateAccount {
    type Request<'a> = (Proof, Serialized<enc::PublicKey>);
    type Response<'a> = Result<(), CreateAccountError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = crate::Identity;

    fn execute(
        context: &mut Self::Context,
        &(proof, enc): &Self::Request<'_>,
        _: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'static> {
        crate::ensure!(proof.verify_profile(), CreateAccountError::InvalidProof);

        let user_id = crypto::hash::new_raw(&proof.pk);
        let entry = context.store_mut().profiles.entry(user_id);

        crate::ensure!(let Entry::Vacant(entry) = entry, CreateAccountError::AlreadyExists);

        entry.insert(FullProfile {
            sign: proof.pk,
            enc,
            action: proof.nonce,
            vault: Vec::new(),
            mail: Vec::new(),
        });
        replicate::<Self>(context, &user_id, &(proof, enc), meta);

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error CreateAccountError {
        InvalidProof => "invalid proof",
        AlreadyExists => "account already exists",
    }
}

pub enum SetVault {}

impl crate::SyncHandler for SetVault {
    type Request<'a> = (Proof, Reminder<'a>);
    type Response<'a> = Result<(), SetVaultError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = crate::Identity;

    fn execute(
        context: &mut Self::Context,
        &(proof, content): &Self::Request<'_>,
        _: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'static> {
        crate::ensure!(proof.verify_profile(), SetVaultError::InvalidProof);

        let identity = crypto::hash::new_raw(&proof.pk);
        let profile = context.store_mut().profiles.get_mut(&identity);

        crate::ensure!(let Some(profile) = profile, SetVaultError::NotFound);

        crate::ensure!(
            advance_nonce(&mut profile.action, proof.nonce),
            SetVaultError::InvalidAction
        );

        profile.vault.clear();
        profile.vault.extend_from_slice(content.0.as_ref());
        replicate::<Self>(context, &identity, &(proof, content), meta);

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error SetVaultError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }
}

pub enum FetchVault {}

impl crate::SyncHandler for FetchVault {
    type Request<'a> = Identity;
    type Response<'a> = Result<(Nonce, Reminder<'a>), FetchVaultError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = Identity;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Self::Response<'a> {
        let profile = context.store_mut().profiles.get(&request);
        crate::ensure!(let Some(profile) = profile, FetchVaultError::NotFound);
        Ok((profile.action, Reminder(profile.vault.as_slice())))
    }
}

component_utils::gen_simple_error! {
    error FetchVaultError {
        NotFound => "account not found",
    }
}

pub enum ReadMail {}

impl crate::SyncHandler for ReadMail {
    type Request<'a> = Proof;
    type Response<'a> = Result<Reminder<'a>, ReadMailError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = crate::Identity;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Self::Response<'a> {
        crate::ensure!(request.verify_profile(), ReadMailError::InvalidProof);

        let profile = context
            .store_mut()
            .profiles
            .get_mut(&crypto::hash::new_raw(&request.pk));
        crate::ensure!(let Some(profile) = profile, ReadMailError::NotFound);
        crate::ensure!(
            advance_nonce(&mut profile.action, request.nonce),
            ReadMailError::InvalidAction
        );
        Ok(Reminder(profile.read_mail()))
    }
}

component_utils::gen_simple_error! {
    error ReadMailError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }
}

pub enum SendMail {}

impl crate::SyncHandler for SendMail {
    type Request<'a> = (Identity, Reminder<'a>);
    type Response<'a> = Result<(), SendMailError>;
    type Context = libp2p::kad::Behaviour<Storage>;
    type Topic = crate::Identity;
    type Event<'a> = Reminder<'a>;

    fn execute(
        context: &mut Self::Context,
        &(identity, Reminder(content)): &Self::Request<'_>,
        events: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'static> {
        let profile = context.store_mut().profiles.get_mut(&identity);
        crate::ensure!(let Some(profile) = profile, SendMailError::NotFound);
        crate::ensure!(
            profile.mail.len() + content.len() < MAIL_BOX_CAP,
            SendMailError::MailboxFull
        );

        profile.mail.extend((content.len() as u16).to_be_bytes());
        profile.mail.extend_from_slice(content);
        replicate::<Self>(context, &identity, &(identity, Reminder(content)), meta);
        events.push(identity, &Reminder(content));

        Ok(())
    }
}

component_utils::gen_simple_error! {
    error SendMailError {
        NotFound => "account not found",
        MailboxFull => "mailbox full",
    }
}