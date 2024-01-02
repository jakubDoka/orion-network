use {
    super::*,
    chat_logic::{Protocol, *},
    component_utils::{arrayvec::ArrayVec, FindAndRemove},
    std::{collections::hash_map::Entry, marker::PhantomData},
};

pub type SyncRetry<H> = RetryBase<Sync<H>, rpc::Event>;
pub type Retry<H> = RetryBase<H, <H as Handler>::Event>;

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
    H::Protocol: TopicProtocol,
    for<'a> &'a E: TryUnwrap<&'a rpc::Event> + TryUnwrap<&'a H::Event>,
    H::Event: 'static,
{
    type Event = E;
    type Protocol = NotFound<H::Protocol>;

    fn execute<'a>(
        sc: Scope<'a>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        let topic: PossibleTopic = <Self::Protocol as TopicProtocol>::extract_topic(&req).into();

        crate::ensure!(sc.cx.is_valid_topic(topic), Ok(NotFoundError::NotFound));

        let mut packet = [0u8; std::mem::size_of::<(u8, Identity)>()];
        match topic {
            PossibleTopic::Profile(identity) if sc.cx.storage.profiles.contains_key(&identity) => {
                return H::execute(sc, req)
                    .map_err(|h| Self::Handling(h, topic, PhantomData))
                    .map(|r| r.map_err(NotFoundError::Inner))
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
                H::resume(h, sc, event)
                    .map_err(|h| Self::Handling(h, topic, ph))
                    .map(|e| e.map_err(NotFoundError::Inner))
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

                            if crypto::hash::from_raw(&profile.sign) != identity {
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
                H::execute(sc, req)
                    .map_err(|h| Self::Handling(h, r.topic, PhantomData))
                    .map(|r| r.map_err(NotFoundError::Inner))
            }
        }
    }
}

pub struct NotFound<T: Protocol>(T);

impl<T: Protocol> Protocol for NotFound<T> {
    type Error = NotFoundError<T::Error>;
    type Request<'a> = T::Request<'a>;
    type Response<'a> = T::Response<'a>;

    const PREFIX: u8 = T::PREFIX;
}

impl<T: TopicProtocol> TopicProtocol for NotFound<T> {
    type Topic = T::Topic;

    fn extract_topic(request: &Self::Request<'_>) -> Self::Topic {
        T::extract_topic(request)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NotFoundError<T> {
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    Inner(T),
}

impl<'a, T: Codec<'a>> Codec<'a> for NotFoundError<T> {
    fn encode(&self, buf: &mut impl codec::Buffer) -> Option<()> {
        match self {
            Self::NotFound => buf.push(0),
            Self::Inner(e) => e.encode(buf),
        }
    }

    fn decode(buf: &mut &'a [u8]) -> Option<Self> {
        match buf.take_first()? {
            0 => Some(Self::NotFound),
            _ => Some(Self::Inner(T::decode(buf)?)),
        }
    }
}
