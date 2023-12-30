use {
    super::*,
    chat_logic::{Protocol, *},
    component_utils::{arrayvec::ArrayVec, FindAndRemove},
    std::{collections::hash_map::Entry, marker::PhantomData, u8},
};

pub trait CanBeMissing: ExtractTopic {
    fn not_found() -> Self::Error;
}

pub struct Restoring {
    topic: PossibleTopic,
    pending: ArrayVec<CallId, { REPLICATION_FACTOR.get() }>,
    request: Vec<u8>,
}

pub enum RetryBase<H, E> {
    Handling(H, PossibleTopic, PhantomData<fn(&E)>),
    Restoring(Restoring),
}

impl<H, E> Handler for RetryBase<H, E>
where
    H: Handler,
    H::Protocol: CanBeMissing,
    for<'a> &'a E: TryUnwrap<&'a rpc::Event> + TryUnwrap<&'a H::Event>,
    for<'a> &'a H::Event: TryUnwrap<&'a rpc::Event>,
    H::Event: 'static,
{
    type Event = E;
    type Protocol = H::Protocol;

    fn execute<'a>(
        sc: Scope<'a>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        let topic: PossibleTopic = <Self::Protocol as ExtractTopic>::extract_topic(&req).into();

        crate::ensure!(sc.cx.is_valid_topic(topic), Ok(H::Protocol::not_found()));

        let mut packet = [0u8; std::mem::size_of::<(u8, Identity)>()];
        match topic {
            PossibleTopic::Profile(identity) if sc.cx.storage.profiles.contains_key(&identity) => {
                return H::execute(sc, req).map_err(|h| Self::Handling(h, topic, PhantomData))
            }
            PossibleTopic::Profile(identity) => {
                (<FetchFullProfile as Protocol>::PREFIX, identity)
                    .encode(&mut packet.as_mut_slice())
                    .expect("always big enough");
            }
            PossibleTopic::Chat(_) => todo!(),
        }

        let us = *sc.cx.swarm.local_peer_id();
        let beh = sc.cx.swarm.behaviour_mut();
        let pending = crate::other_replicators_for(&beh.dht.table, topic, us)
            .filter_map(|peer| beh.rpc.request(peer, packet).ok())
            .collect();

        Err(Self::Restoring(Restoring {
            topic,
            pending,
            request: req.to_bytes(),
        }))
    }

    fn resume<'a>(self, sc: Scope<'a>, event: &'a Self::Event) -> HandlerResult<'a, Self> {
        match self {
            Self::Handling(h, topic, ph) => {
                crate::ensure!(let Ok(event) = TryUnwrap::<&'a H::Event>::try_unwrap(event), Self::Handling(h, topic, ph));
                H::resume(h, sc, event).map_err(|h| Self::Handling(h, topic, ph))
            }
            Self::Restoring(mut r) => {
                crate::ensure!(let Ok(&rpc::Event::Response(_, call, ref res)) = event.try_unwrap(), Self::Restoring(r));
                crate::ensure!(
                    r.pending.find_and_remove(|&c| c == call).is_some(),
                    Self::Restoring(r)
                );

                match r.topic {
                    PossibleTopic::Profile(identity) => match res {
                        Ok((request, ..)) => 'a: {
                            let Some(Ok(profile)) = ProtocolResult::<'a, FetchFullProfile>::decode(
                                &mut request.as_slice(),
                            ) else {
                                break 'a;
                            };

                            if crypto::hash::new_raw(&profile.sign) != identity {
                                break 'a;
                            }

                            if !profile.is_valid() {
                                break 'a;
                            }

                            let entry = sc.cx.storage.profiles.entry(identity);
                            if let Entry::Occupied(existing) = &entry
                                && existing.get().vault_version >= profile.vault_version
                            {
                                break 'a;
                            }

                            entry.insert_entry(profile.into());
                        }
                        Err(e) => {
                            log::warn!("rpc failed: {e}");
                        }
                    },
                    PossibleTopic::Chat(_) => todo!(),
                }

                crate::ensure!(r.pending.is_empty(), Self::Restoring(r));
                let req = <Self::Protocol as Protocol>::Request::decode(&mut r.request.as_slice())
                    .expect("always valid");
                H::execute(sc, req).map_err(|h| Self::Handling(h, r.topic, PhantomData))
            }
        }
    }
}
