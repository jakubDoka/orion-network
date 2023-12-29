use {
    super::*,
    crate::advance_nonce,
    chat_logic::{Protocol, *},
    component_utils::{arrayvec::ArrayVec, Reminder},
    std::collections::hash_map::Entry,
};

const MAIL_BOX_CAP: usize = 1024 * 1024;

impl SyncHandler for FetchProfile {
    fn execute<'a>(cx: Scope<'a>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        cx.storage
            .profiles
            .get(&request)
            .map(|profile| profile.into())
            .ok_or(FetchProfileError::NotFound)
    }
}

impl SyncHandler for FetchFullProfile {
    fn execute<'a>(sc: Scope<'a>, req: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        sc.cx
            .storage
            .profiles
            .get(&req)
            .map(Into::into)
            .ok_or(FetchProfileError::NotFound)
    }
}

impl SyncHandler for CreateProfile {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (proof, enc, Reminder(vault)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_mail(), CreateAccountError::InvalidProof);

        let user_id = crypto::hash::new_raw(&proof.pk);
        let entry = cx.storage.profiles.entry(user_id);

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

impl SyncHandler for SetVault {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (proof, Reminder(content)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_vault(content), SetVaultError::InvalidProof);

        let identity = crypto::hash::new_raw(&proof.pk);
        let profile = cx.storage.profiles.get_mut(&identity);

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

impl SyncHandler for FetchVault {
    fn execute<'a>(sc: Scope<'a>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        let profile = sc.cx.storage.profiles.get(&request);
        crate::ensure!(let Some(profile) = profile, FetchVaultError::NotFound);
        Ok((
            profile.vault_version,
            profile.mail_action,
            Reminder(profile.vault.as_slice()),
        ))
    }
}

impl SyncHandler for ReadMail {
    fn execute<'a>(sc: Scope<'a>, request: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        crate::ensure!(request.verify_mail(), ReadMailError::InvalidProof);
        let store = sc.cx.storage;
        let identity = crypto::hash::new_raw(&request.pk);
        let profile = store.profiles.get_mut(&identity);
        crate::ensure!(let Some(profile) = profile, ReadMailError::NotFound);
        crate::ensure!(
            advance_nonce(&mut profile.mail_action, request.nonce),
            ReadMailError::InvalidAction
        );
        store.online.insert(identity, sc.origin);
        Ok(Reminder(profile.read_mail()))
    }
}

pub struct SendingMail {
    dm: CallId,
    for_who: Identity,
}

impl SendingMail {
    fn clear_presence(self, mut cx: Scope) -> HandlerResult<SendMail> {
        cx.storage.online.remove(&self.for_who);
        Ok(Ok(()))
    }

    fn pop_pushed_mail(self, mut cx: Scope) -> HandlerResult<SendMail> {
        if let Some(profile) = cx.storage.profiles.get_mut(&self.for_who) {
            profile.mail.clear();
        };
        Ok(Err(SendMailError::SentDirectly))
    }
}

pub struct RestoringProfile {
    pending: ArrayVec<CallId, { REPLICATION_FACTOR.get() }>,
}

pub enum SendMail {
    Sending(SendingMail),
    Restoring(RestoringProfile),
}

impl Handler for SendMail {
    type Event = rpc::Event;
    type Protocol = chat_logic::SendMail;

    fn execute<'a>(
        sc: Scope<'a>,
        req @ (for_who, Reminder(content)): <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        let Some(profile) = sc.cx.storage.profiles.get_mut(&for_who) else {
            crate::ensure!(
                sc.cx.is_valid_topic(for_who.into()),
                Ok(SendMailError::NotFound)
            );

            let mut packet = [0u8; std::mem::size_of::<(u8, Identity)>()];
            (<FetchFullProfile as Protocol>::PREFIX, for_who).encode(&mut packet.as_mut_slice());
            let my_id = *sc.cx.swarm.local_peer_id();
            let beh = sc.cx.swarm.behaviour_mut();
            let pending = beh
                .dht
                .table
                .closest(&for_who.0)
                .take(REPLICATION_FACTOR.get() + 1)
                .filter(|peer| peer.peer_id() != my_id)
                .filter_map(|peer| beh.rpc.request(peer.peer_id(), packet).ok())
                .collect();

            return Err(Self::Restoring(RestoringProfile { pending }));
        };

        crate::ensure!(
            profile.mail.len() + content.len() < MAIL_BOX_CAP,
            Ok(SendMailError::MailboxFull)
        );

        let Entry::Occupied(online_in) = sc.cx.storage.online.entry(for_who) else {
            profile.push_mail(content);
            return Ok(Ok(()));
        };

        match *online_in.get() {
            RequestOrigin::Client(p) => {
                crate::ensure!(
                    RequestOrigin::Client(p) != sc.origin,
                    Ok(SendMailError::SendingToSelf)
                );
                crate::ensure!(
                    crate::push_notification(sc.cx.clients, for_who, Reminder(content), p),
                    Ok(SendMailError::SentDirectly)
                );

                online_in.remove();
                profile.push_mail(content);
                Ok(Ok(()))
            }
            RequestOrigin::Server(peer) => {
                profile.push_mail(content);
                if matches!(sc.origin, RequestOrigin::Server(_)) {
                    online_in.remove();
                    return Ok(Ok(()));
                }

                let packet = (sc.prefix, req).to_bytes();
                if let Ok(dm) = sc.cx.swarm.behaviour_mut().rpc.request(peer, packet) {
                    Err(Self::Sending(SendingMail { dm, for_who }))
                } else {
                    Ok(Ok(()))
                }
            }
        }
    }

    fn resume<'a>(self, cx: Scope<'a>, enent: &'a Self::Event) -> HandlerResult<'a, Self> {
        match self {
            Self::Sending(s) => {
                crate::ensure!(let rpc::Event::Response(_, call, res) = enent, Self::Sending(s));
                crate::ensure!(*call == s.dm, Self::Sending(s));

                let mut request = match res {
                    Ok((request, ..)) => request.as_slice(),
                    Err(_) => return s.clear_presence(cx),
                };

                if let Some(Err(SendMailError::SentDirectly)) =
                    ProtocolResult::<'a, Self::Protocol>::decode(&mut request)
                {
                    s.pop_pushed_mail(cx)
                } else {
                    s.clear_presence(cx)
                }
            }
            Self::Restoring(_) => todo!(),
        }
    }
}
