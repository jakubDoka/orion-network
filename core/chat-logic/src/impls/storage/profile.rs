use {
    super::Storage,
    crate::{advance_nonce, HandlerResult, Identity, Nonce, Proof},
    component_utils::Reminder,
    crypto::{enc, sign, Serialized},
    libp2p::PeerId,
    std::collections::hash_map::Entry,
};

const MAIL_BOX_CAP: usize = 1024 * 1024;

component_utils::protocol! {'a:
    #[derive(Clone)]
    struct Profile {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        last_sig: Serialized<sign::Signature>,
        action: Nonce,
        vault: Vec<u8>,
        mail: Vec<u8>,
        online: Option<PeerId>,
    }
}

impl Profile {
    fn read_mail(&mut self) -> &[u8] {
        let slice = unsafe { std::mem::transmute(self.mail.as_slice()) };
        unsafe { self.mail.set_len(0) };
        slice
    }
}

pub enum FetchProfile {}

impl crate::SyncHandler for FetchProfile {
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = FetchProfileError;
    type Request<'a> = Identity;
    type Response<'a> = FetchProfileResp;
    type Topic = Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
        context
            .store_mut()
            .profiles
            .get(request)
            .map(|profile| profile.into())
            .ok_or(FetchProfileError::NotFound)
    }

    fn extract_topic(req: &Self::Request<'_>) -> Option<Self::Topic> {
        Some(*req)
    }
}

component_utils::gen_simple_error! {
    error FetchProfileError {
        NotFound => "account not found",
    }
}

component_utils::protocol! {'a:
    struct FetchProfileResp {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
    }
}

impl From<&Profile> for FetchProfileResp {
    fn from(profile: &Profile) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
        }
    }
}

pub enum CreateAccount {}

impl crate::SyncHandler for CreateAccount {
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = CreateAccountError;
    type Request<'a> = (Proof, Serialized<enc::PublicKey>, Reminder<'a>);
    type Response<'a> = ();
    type Topic = crate::Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        &(proof, enc, vault): &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
        crate::ensure!(proof.verify_profile(), CreateAccountError::InvalidProof);

        let user_id = crypto::hash::new_raw(&proof.pk);
        let replicating = context.store_mut().dont_replicate;
        let entry = context.store_mut().profiles.entry(user_id);

        match entry {
            Entry::Vacant(entry) => {
                entry.insert(Profile {
                    sign: proof.pk,
                    enc,
                    last_sig: proof.signature,
                    action: proof.nonce,
                    vault: vault.0.to_vec(),
                    mail: Vec::new(),
                    online: None,
                });
                Ok(())
            }
            Entry::Occupied(mut entry) if replicating && entry.get().action < proof.nonce => {
                let account = entry.get_mut();
                account.action = proof.nonce;
                account.last_sig = proof.signature;
                account.vault.clear();
                account.vault.extend(vault.0);
                Ok(())
            }
            _ => Err(CreateAccountError::AlreadyExists),
        }
    }

    fn extract_topic(request: &Self::Request<'_>) -> Option<Self::Topic> {
        Some(crypto::hash::new_raw(&request.0.pk))
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
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = SetVaultError;
    type Request<'a> = (Proof, Reminder<'a>);
    type Response<'a> = ();
    type Topic = crate::Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        &(proof, content): &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
        crate::ensure!(proof.verify_profile(), SetVaultError::InvalidProof);

        let identity = crypto::hash::new_raw(&proof.pk);
        let profile = context.store_mut().profiles.get_mut(&identity);

        crate::ensure!(let Some(profile) = profile, SetVaultError::NotFound);

        crate::ensure!(
            advance_nonce(&mut profile.action, proof.nonce),
            SetVaultError::InvalidAction
        );
        profile.last_sig = proof.signature;

        profile.vault.clear();
        profile.vault.extend_from_slice(content.0.as_ref());

        Ok(())
    }

    fn extract_topic(request: &Self::Request<'_>) -> Option<Self::Topic> {
        Some(crypto::hash::new_raw(&request.0.pk))
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
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = FetchVaultError;
    type Request<'a> = Identity;
    type Response<'a> = (Nonce, Reminder<'a>);
    type Topic = Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
        let profile = context.store_mut().profiles.get(request);
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
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = ReadMailError;
    type Request<'a> = Proof;
    type Response<'a> = Reminder<'a>;
    type Topic = crate::Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
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

    fn extract_topic(request: &Self::Request<'_>) -> Option<Self::Topic> {
        Some(crypto::hash::new_raw(&request.pk))
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
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = SendMailError;
    type Event<'a> = Reminder<'a>;
    type Request<'a> = (Identity, Reminder<'a>);
    type Response<'a> = ();
    type Topic = crate::Identity;

    fn execute<'a>(
        context: PassedContext<'a, Self>,
        &(identity, Reminder(content)): &Self::Request<'a>,
        events: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> HandlerResult<'a, Self> {
        let profile = context.store_mut().profiles.get_mut(&identity);

        crate::ensure!(let Some(profile) = profile, SendMailError::NotFound);
        crate::ensure!(
            profile.mail.len() + content.len() < MAIL_BOX_CAP,
            SendMailError::MailboxFull
        );

        profile.mail.extend((content.len() as u16).to_be_bytes());
        profile.mail.extend_from_slice(content);
        events.push(identity, &Reminder(content));

        Ok(())
    }

    fn extract_topic(request: &Self::Request<'_>) -> Option<Self::Topic> {
        Some(request.0)
    }
}

component_utils::gen_simple_error! {
    error SendMailError {
        NotFound => "account not found",
        MailboxFull => "mailbox full",
    }
}
