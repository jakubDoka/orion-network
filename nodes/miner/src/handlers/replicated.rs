use {
    super::{HandlerTypes, ProvideKadAndRpc, Sync, TryUnwrap},
    crate::{Handler, REPLICATION_FACTOR},
    chat_logic::{ExtractTopic, PossibleTopic, Protocol, ReplError},
    component_utils::{Codec, FindAndRemove},
    rpc::CallId,
};

pub type SyncRepl<H> = ReplBase<Sync<H>, rpc::Event>;
pub type Repl<H> = ReplBase<H, <H as HandlerTypes>::Event>;

pub enum ReplBase<H, E> {
    Resolving(H, PossibleTopic, Vec<u8>),
    Replicating {
        response: Vec<u8>,
        ongoing: Vec<CallId>,
        matched: usize,
        phantom: std::marker::PhantomData<fn(E)>,
    },
}

impl<H, E> ReplBase<H, E> {
    pub fn new_replicating<C: ProvideKadAndRpc>(
        response: Vec<u8>,
        request: Vec<u8>,
        topic: PossibleTopic,
        cx: &mut C,
    ) -> Self {
        let (kad, rpc) = cx.kad_and_rpc_mut();
        let ongoing = kad
            .get_closest_local_peers(&topic.to_bytes().into())
            .take(REPLICATION_FACTOR.get())
            .filter_map(|peer| rpc.request(*peer.preimage(), request.as_slice()).ok())
            .collect();

        Self::Replicating {
            response,
            ongoing,
            matched: 0,
            phantom: std::marker::PhantomData,
        }
    }
}

impl<H: HandlerTypes, E> HandlerTypes for ReplBase<H, E> {
    type Event = E;
    type Protocol = chat_logic::Repl<H::Protocol>;
}

impl<C, H, E> Handler<C> for ReplBase<H, E>
where
    C: ProvideKadAndRpc,
    H: Handler<C>,
    H::Protocol: ExtractTopic,
    for<'a> &'a E: TryUnwrap<&'a rpc::Event> + TryUnwrap<&'a H::Event>,
{
    fn execute<'a>(
        mut scope: super::Scope<'a, C>,
        req: <Self::Protocol as chat_logic::Protocol>::Request<'_>,
    ) -> super::HandlerResult<'a, Self> {
        let request = (<Self::Protocol as Protocol>::PREFIX, &req).to_bytes();
        let topic = <H::Protocol as ExtractTopic>::extract_topic(&req);
        let response = match H::execute(scope.reborrow(), req) {
            Ok(Ok(r)) => Ok::<_, ()>(r).to_bytes(),
            Ok(Err(e)) => return Ok(Err(ReplError::Inner(e))),
            Err(e) => return Err(Self::Resolving(e, topic.into(), request)),
        };

        Err(Self::new_replicating(
            response,
            request,
            topic.into(),
            scope.cx,
        ))
    }

    fn resume<'a>(
        mut self,
        mut cx: super::Scope<'a, C>,
        event: &'a Self::Event,
    ) -> super::HandlerResult<'a, Self> {
        let (response, ongoing, matched) = match self {
            ReplBase::Resolving(handler, topic, request) => {
                let response = match handler.resume(
                    cx.reborrow(),
                    event
                        .try_unwrap()
                        .ok()
                        .expect("we always use one of the provided type aliases"),
                ) {
                    Ok(Ok(r)) => Ok::<_, ()>(r).to_bytes(),
                    Ok(Err(e)) => return Ok(Err(ReplError::Inner(e))),
                    Err(h) => return Err(Self::Resolving(h, topic, request)),
                };

                return Err(Self::new_replicating(response, request, topic, cx.cx));
            }
            ReplBase::Replicating {
                ref response,
                ref mut ongoing,
                ref mut matched,
                ..
            } => (response, ongoing, matched),
        };

        crate::ensure!(let Ok(rpc::Event::Response(res)) = TryUnwrap::<&rpc::Event>::try_unwrap(event), self);

        log::debug!("rpc event: {:?}", res);
        match res {
            Ok((_, call, remote_resp, _)) => 'a: {
                if ongoing.find_and_remove(|c| c == call).is_none() {
                    break 'a;
                }

                *matched += (remote_resp.as_slice() == response.as_slice()) as usize;

                if *matched > REPLICATION_FACTOR.get() / 2 {
                    let Some(resp) = Codec::decode(&mut remote_resp.as_slice()) else {
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

        if ongoing.len() + *matched < REPLICATION_FACTOR.get() / 2 {
            return Ok(Err(ReplError::NoMajority));
        }

        Err(self)
    }
}
