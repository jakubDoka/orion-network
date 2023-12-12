use {
    crate::{Context, Handler, SearchPeers, Storage, SubContext, SyncHandler, REPLICATION_FACTOR},
    component_utils::{codec, Codec, FindAndRemove},
    rpc::CallId,
};

pub struct ReplContext<'a> {
    pub kad: &'a mut libp2p::kad::Behaviour<Storage>,
    pub rpc: &'a mut rpc::Behaviour,
}

pub enum ToSwarm {
    Kad(libp2p::kad::Event),
    Rpc(rpc::Event),
}

impl From<libp2p::kad::Event> for ToSwarm {
    fn from(event: libp2p::kad::Event) -> Self {
        Self::Kad(event)
    }
}

impl<'b> Context for ReplContext<'b> {
    type Borrow<'a> = ReplContext<'a>;
    type ToSwarm = ToSwarm;
}

impl<'a> SubContext<libp2p::kad::Behaviour<Storage>> for ReplContext<'a> {
    fn fragment(&mut self) -> <libp2p::kad::Behaviour<Storage> as Context>::Borrow<'_> {
        self.kad
    }

    fn try_unpack_event(
        event: Self::ToSwarm,
    ) -> Result<<libp2p::kad::Behaviour<Storage> as Context>::ToSwarm, Self::ToSwarm> {
        match event {
            ToSwarm::Kad(event) => Ok(event),
            event => Err(event),
        }
    }
}

pub trait ReplicationHandler: SyncHandler<Context = libp2p::kad::Behaviour<Storage>> {}
impl<H> ReplicationHandler for H where H: SyncHandler<Context = libp2p::kad::Behaviour<Storage>> {}

pub struct Replicated<H> {
    request: Vec<u8>,
    response: Vec<u8>,
    stage: Stage,
    phantom: std::marker::PhantomData<H>,
}

enum Stage {
    FindingPeers(SearchPeers),
    SendingRpcs {
        ongoing: Vec<CallId>,
        matched: usize,
    },
}

impl<H> Handler for Replicated<H>
where
    H: ReplicationHandler,
{
    type Context = ReplContext<'static>;
    type Error = ReplicationError<H::Error>;
    type Event<'a> = H::Event<'a>;
    type Request<'a> = H::Request<'a>;
    type Response<'a> = H::Response<'a>;
    type Topic = H::Topic;

    fn spawn<'a>(
        context: crate::PassedContext<'a, Self>,
        request: &Self::Request<'a>,
        dispatch: &mut crate::EventDispatch<Self>,
        meta @ (prefix, ..): crate::RequestMeta,
    ) -> Result<crate::HandlerResult<'a, Self>, Self> {
        let r = match H::execute(
            // SAFETY: rust is stupid in this case since we get rid of all potentially borrowed
            // data in the statement
            unsafe { std::mem::transmute(&mut *context.kad) },
            request,
            dispatch.cast(),
            meta,
        ) {
            Ok(r) if context.kad.store_mut().dont_replicate => return Ok(Ok(r)),
            Ok(r) => Ok::<_, ()>(r).to_bytes(),
            Err(e) => return Ok(Err(ReplicationError::Inner(e))),
        };

        let topic = H::extract_topic(request).unwrap();
        log::info!("replicating {:?}", prefix);

        Err(Self {
            request: (prefix, request).to_bytes(),
            response: r,
            stage: Stage::FindingPeers(SearchPeers::new(context.kad, &topic)),
            phantom: std::marker::PhantomData,
        })
    }

    fn try_complete<'a>(
        self,
        context: crate::PassedContext<'a, Self>,
        _: &mut crate::EventDispatch<Self>,
        event: &'a <Self::Context as Context>::ToSwarm,
    ) -> Result<crate::HandlerResult<'a, Self>, Self> {
        match (event, self.stage) {
            (ToSwarm::Kad(e), Stage::FindingPeers(peers)) => {
                log::info!("kad event: {:?}", e);
                let peers = match peers.try_complete(e) {
                    Ok(p) => p,
                    Err(s) => {
                        return Err(Self {
                            stage: Stage::FindingPeers(s),
                            ..self
                        })
                    }
                };

                let calls = peers
                    .iter()
                    .map(|&peer| context.rpc.request(peer, self.request.as_slice()))
                    .collect();
                Err(Self {
                    stage: Stage::SendingRpcs {
                        ongoing: calls,
                        matched: 0,
                    },
                    ..self
                })
            }
            (
                ToSwarm::Rpc(rpc::Event::Response(res)),
                Stage::SendingRpcs {
                    mut ongoing,
                    mut matched,
                },
            ) => {
                log::info!("rpc event: {:?}", res);
                match res {
                    Ok((_, call, response, _)) => {
                        if ongoing.find_and_remove(|c| c == call).is_none() {
                            return Err(Self {
                                stage: Stage::SendingRpcs { ongoing, matched },
                                ..self
                            });
                        }

                        matched += (self.response.as_slice() == response.as_slice()) as usize;

                        if matched > REPLICATION_FACTOR.get() / 2 {
                            return Ok(Ok(Codec::decode(&mut response.as_slice()).unwrap()));
                        }
                    }
                    Err((failed, e)) => {
                        for f in failed {
                            ongoing.find_and_remove(|c| c == f);
                        }
                        log::warn!("rpc failed: {}", e);
                    }
                }

                if ongoing.len() + matched < REPLICATION_FACTOR.get() / 2 {
                    return Ok(Err(ReplicationError::NoMajority));
                }

                Err(Self {
                    stage: Stage::SendingRpcs { ongoing, matched },
                    ..self
                })
            }
            (_, stage) => Err(Self { stage, ..self }),
        }
    }

    fn extract_topic(r: &Self::Request<'_>) -> Option<Self::Topic> {
        H::extract_topic(r)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplicationError<T> {
    #[error("no majority")]
    NoMajority,
    #[error(transparent)]
    Inner(T),
}

impl<'a, T: Codec<'a>> Codec<'a> for ReplicationError<T> {
    fn encode(&self, buf: &mut impl codec::Buffer) -> Option<()> {
        match self {
            Self::NoMajority => buf.push(0),
            Self::Inner(e) => {
                buf.push(1)?;
                e.encode(buf)
            }
        }
    }

    fn decode(buf: &mut &'a [u8]) -> Option<Self> {
        match buf.take_first()? {
            0 => Some(Self::NoMajority),
            1 => Some(Self::Inner(T::decode(buf)?)),
            _ => None,
        }
    }
}
