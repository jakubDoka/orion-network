use {
    super::{ProvideKad, ProvideRpc, SearchPeers},
    crate::{Handler, SyncHandler, REPLICATION_FACTOR},
    chat_logic::{ExtractTopic, Protocol, ReplError},
    component_utils::{Codec, FindAndRemove},
    rpc::CallId,
};

pub enum ReplEvent {
    Kad(libp2p::kad::Event),
    Rpc(rpc::Event),
}

pub struct Repl<H> {
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

impl<C: ProvideKad + ProvideRpc, H: SyncHandler<C> + ExtractTopic> Handler<C> for Repl<H> {
    type Event = ReplEvent;
    type Protocol = chat_logic::Repl<H>;

    fn execute<'a>(
        mut scope: super::Scope<'a, C>,
        req: <Self::Protocol as chat_logic::Protocol>::Request<'_>,
    ) -> super::HandlerResult<'a, Self, C> {
        let request = (<Self::Protocol as Protocol>::PREFIX, &req).to_bytes();
        let topic = H::extract_topic(&req);
        let r = match H::execute(scope.reborrow(), req) {
            Ok(r) => Ok::<_, ()>(r).to_bytes(),
            Err(e) => return Ok(Err(ReplError::Inner(e))),
        };

        Err(Self {
            request,
            response: r,
            stage: Stage::FindingPeers(SearchPeers::new(scope.kad_mut(), topic)),
            phantom: std::marker::PhantomData,
        })
    }

    fn resume<'a>(
        self,
        mut cx: super::Scope<'a, C>,
        event: &'a Self::Event,
    ) -> super::HandlerResult<'a, Self, C> {
        match (event, self.stage) {
            (ReplEvent::Kad(e), Stage::FindingPeers(peers)) => {
                log::debug!("kad event: {:?}", e);
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
                    .filter_map(|&peer| cx.rpc_mut().request(peer, self.request.as_slice()).ok())
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
                ReplEvent::Rpc(rpc::Event::Response(res)),
                Stage::SendingRpcs {
                    mut ongoing,
                    mut matched,
                },
            ) => {
                log::debug!("rpc event: {:?}", res);
                match res {
                    Ok((_, call, response, _)) => 'a: {
                        if ongoing.find_and_remove(|c| c == call).is_none() {
                            break 'a;
                        }

                        matched += (self.response.as_slice() == response.as_slice()) as usize;

                        if matched > REPLICATION_FACTOR.get() / 2 {
                            let Some(resp) = Codec::decode(&mut response.as_slice()) else {
                                return Ok(Err(ReplError::InvalidResponse));
                            };

                            return Ok(resp);
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
                    return Ok(Err(ReplError::NoMajority));
                }

                Err(Self {
                    stage: Stage::SendingRpcs { ongoing, matched },
                    ..self
                })
            }
            (_, stage) => Err(Self { stage, ..self }),
        }
    }
}
