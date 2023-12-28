use {
    super::*,
    crate::{advance_nonce, Nonce},
    chat_logic::*,
    component_utils::Reminder,
    crypto::{enc, sign, Serialized},
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
        (proof, enc, Reminder(vault)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_mail(), CreateAccountError::InvalidProof);

        let user_id = crypto::hash::new_raw(&proof.pk);
        let entry = cx.store_mut().profiles.entry(user_id);

        match entry {
            Entry::Vacant(entry) => {
                entry.insert(Profile {
                    sign: proof.pk,
                    enc,
                    last_sig: proof.signature,
                    vault_version: proof.nonce,
                    mail_action: proof.nonce,
                    vault: vault.to_vec(),
                    mail: Vec::new(),
                    online_in: None,
                });
                Ok(())
            }
            Entry::Occupied(mut entry) if entry.get().vault_version < proof.nonce => {
                let account = entry.get_mut();
                account.vault_version = proof.nonce;
                account.last_sig = proof.signature;
                account.vault.clear();
                account.vault.extend(vault);
                Ok(())
            }
            _ => Err(CreateAccountError::AlreadyExists),
        }
    }
}

impl<C: ProvideStorage> SyncHandler<C> for SetVault {
    fn execute<'a>(
        mut cx: Scope<'a, C>,
        (proof, Reminder(content)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_vault(content), SetVaultError::InvalidProof);

        let identity = crypto::hash::new_raw(&proof.pk);
        let profile = cx.store_mut().profiles.get_mut(&identity);

        crate::ensure!(let Some(profile) = profile, SetVaultError::NotFound);

        crate::ensure!(
            advance_nonce(&mut profile.vault_version, proof.nonce),
            SetVaultError::InvalidAction
        );
        profile.last_sig = proof.signature;

        profile.vault.clear();
        profile.vault.extend_from_slice(content.as_ref());

        Ok(())
    }
}

impl<C: ProvideStorage> SyncHandler<C> for FetchVault {
    fn execute<'a>(sc: Scope<'a, C>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        let profile = sc.cx.store_mut().profiles.get(&request);
        crate::ensure!(let Some(profile) = profile, FetchVaultError::NotFound);
        Ok((
            profile.vault_version,
            profile.mail_action,
            Reminder(profile.vault.as_slice()),
        ))
    }
}

impl<C: ProvideStorage> SyncHandler<C> for ReadMail {
    fn execute<'a>(sc: Scope<'a, C>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        crate::ensure!(request.verify_mail(), ReadMailError::InvalidProof);
        let profile = sc
            .cx
            .store_mut()
            .profiles
            .get_mut(&crypto::hash::new_raw(&request.pk));
        crate::ensure!(let Some(profile) = profile, ReadMailError::NotFound);
        crate::ensure!(
            advance_nonce(&mut profile.mail_action, request.nonce),
            ReadMailError::InvalidAction
        );
        profile.online_in = Some(sc.origin);
        Ok(Reminder(profile.read_mail()))
    }
}

pub struct SendMail {
    dm: CallId,
    for_who: Identity,
}

impl HandlerTypes for SendMail {
    type Event = rpc::Event;
    type Protocol = chat_logic::SendMail;
}

impl SendMail {
    pub fn clear_presence<C: ProvideStorage>(self, mut cx: Scope<C>) -> HandlerResult<Self> {
        if let Some(profile) = cx.store_mut().profiles.get_mut(&self.for_who) {
            profile.online_in = None;
        }
        Ok(Ok(()))
    }

    pub fn pop_pushed_mail(self, mut cx: Scope<'_, impl ProvideStorage>) -> HandlerResult<Self> {
        if let Some(profile) = cx.store_mut().profiles.get_mut(&self.for_who) {
            profile.mail.clear();
        };
        Ok(Err(SendMailError::SentDirectly))
    }
}

impl<C: DirectedEventEmmiter<Identity> + ProvideStorage + ProvideRpc> Handler<C> for SendMail {
    fn execute<'a>(
        mut sc: Scope<'a, C>,
        req @ (for_who, Reminder(content)): <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        let profile = sc.cx.store_mut().profiles.get_mut(&for_who);

        crate::ensure!(let Some(profile) = profile, Ok(SendMailError::NotFound));
        crate::ensure!(
            profile.mail.len() + content.len() < MAIL_BOX_CAP,
            Ok(SendMailError::MailboxFull)
        );

        let Some(online_in) = profile.online_in else {
            profile.push_mail(content);
            return Ok(Ok(()));
        };

        match online_in {
            RequestOrigin::Client(p) => {
                crate::ensure!(online_in != sc.origin, Ok(SendMailError::SendingToSelf));
                crate::ensure!(
                    !sc.push(for_who, Reminder(content), p),
                    Ok(SendMailError::SentDirectly)
                );

                let p = sc
                    .store_mut()
                    .profiles
                    .get_mut(&for_who)
                    .expect("we checked");
                p.online_in = None;
                p.push_mail(content);
                Ok(Ok(()))
            }
            RequestOrigin::Miner(peer) => {
                profile.push_mail(content);
                if matches!(sc.origin, RequestOrigin::Miner(_)) {
                    profile.online_in = None;
                    return Ok(Ok(()));
                }

                let packet = (sc.prefix, req).to_bytes();
                if let Ok(dm) = sc.rpc_mut().request(peer, packet) {
                    Err(Self { dm, for_who })
                } else {
                    Ok(Ok(()))
                }
            }
        }
    }

    fn resume<'a>(self, cx: Scope<'a, C>, enent: &'a Self::Event) -> HandlerResult<'a, Self> {
        crate::ensure!(let rpc::Event::Response(_, call, res) = enent, self);
        crate::ensure!(*call == self.dm, self);

        let mut request = match res {
            Ok((request, ..)) => request.as_slice(),
            Err(_) => return self.clear_presence(cx),
        };

        if let Some(Err(SendMailError::SentDirectly)) =
            ProtocolResult::<'a, Self::Protocol>::decode(&mut request)
        {
            self.pop_pushed_mail(cx)
        } else {
            self.clear_presence(cx)
        }
    }
}

component_utils::protocol! {'a:
    #[derive(Clone)]
    struct Profile {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        last_sig: Serialized<sign::Signature>,
        vault_version: Nonce,
        mail_action: Nonce,
        vault: Vec<u8>,
        mail: Vec<u8>,
        online_in: Option<RequestOrigin>,
    }
}

impl Profile {
    fn read_mail(&mut self) -> &[u8] {
        let slice = unsafe { std::mem::transmute(self.mail.as_slice()) };
        unsafe { self.mail.set_len(0) };
        slice
    }

    fn push_mail(&mut self, content: &[u8]) {
        self.mail.extend((content.len() as u16).to_be_bytes());
        self.mail.extend_from_slice(content);
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
