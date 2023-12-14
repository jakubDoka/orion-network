use {
    super::*,
    crate::{advance_nonce, Nonce},
    chat_logic::*,
    component_utils::Reminder,
    crypto::{enc, sign, Serialized},
    libp2p::PeerId,
    std::collections::hash_map::Entry,
};

const MAIL_BOX_CAP: usize = 1024 * 1024;

impl<C: ProvideStorage> SyncHandler<C> for FetchProfile {
    fn execute<'a>(mut cx: Scope<'a, C>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        cx.store_mut()
            .profiles
            .get(&request)
            .map(|profile| profile.into())
            .ok_or(FetchProfileError::NotFound)
    }
}

impl<C: ProvideStorage> SyncHandler<C> for CreateProfile {
    fn execute<'a>(
        mut cx: Scope<'a, C>,
        (proof, enc, vault): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_profile(), CreateAccountError::InvalidProof);

        let user_id = crypto::hash::new_raw(&proof.pk);
        let entry = cx.store_mut().profiles.entry(user_id);

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
            Entry::Occupied(mut entry) if entry.get().action < proof.nonce => {
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
}

impl<C: ProvideStorage> SyncHandler<C> for SetVault {
    fn execute<'a>(
        mut cx: Scope<'a, C>,
        (proof, content): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_profile(), SetVaultError::InvalidProof);

        let identity = crypto::hash::new_raw(&proof.pk);
        let profile = cx.store_mut().profiles.get_mut(&identity);

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
}

impl<C: ProvideStorage> SyncHandler<C> for FetchVault {
    fn execute<'a>(sc: Scope<'a, C>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        let profile = sc.cx.store_mut().profiles.get(&request);
        crate::ensure!(let Some(profile) = profile, FetchVaultError::NotFound);
        Ok((profile.action, Reminder(profile.vault.as_slice())))
    }
}

impl<C: ProvideStorage> SyncHandler<C> for ReadMail {
    fn execute<'a>(sc: Scope<'a, C>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        crate::ensure!(request.verify_profile(), ReadMailError::InvalidProof);

        let profile = sc
            .cx
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

impl<C: EventEmmiter<Identity> + ProvideStorage> SyncHandler<C> for SendMail {
    fn execute<'a>(
        mut cx: Scope<'a, C>,
        (identity, Reminder(content)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        let profile = cx.store_mut().profiles.get_mut(&identity);

        crate::ensure!(let Some(profile) = profile, SendMailError::NotFound);
        crate::ensure!(
            profile.mail.len() + content.len() < MAIL_BOX_CAP,
            SendMailError::MailboxFull
        );

        profile.mail.extend((content.len() as u16).to_be_bytes());
        profile.mail.extend_from_slice(content);
        cx.push(identity, Reminder(content));

        Ok(())
    }
}

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

impl From<&Profile> for FetchProfileResp {
    fn from(profile: &Profile) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
        }
    }
}

