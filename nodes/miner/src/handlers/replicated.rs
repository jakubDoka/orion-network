use {
    super::ProvideKadAndRpc,
    crate::{Handler, SyncHandler, REPLICATION_FACTOR},
    chat_logic::{ExtractTopic, Protocol, ReplError},
    component_utils::{Codec, FindAndRemove},
    rpc::CallId,
};

pub struct Repl<H> {
    response: Vec<u8>,
    ongoing: Vec<CallId>,
    matched: usize,
    phantom: std::marker::PhantomData<H>,
}

impl<C: ProvideKadAndRpc, H: SyncHandler<C> + ExtractTopic> Handler<C> for Repl<H> {
    type Event = rpc::Event;
    type Protocol = chat_logic::Repl<H>;

    fn execute<'a>(
        mut scope: super::Scope<'a, C>,
        req: <Self::Protocol as chat_logic::Protocol>::Request<'_>,
    ) -> super::HandlerResult<'a, Self, C> {
        let request = (<Self::Protocol as Protocol>::PREFIX, &req).to_bytes();
        let topic = H::extract_topic(&req);
        let response = match H::execute(scope.reborrow(), req) {
            Ok(r) => Ok::<_, ()>(r).to_bytes(),
            Err(e) => return Ok(Err(ReplError::Inner(e))),
        };

        let (kad, rpc) = scope.kad_and_rpc_mut();
        let calls = kad
            .get_closest_local_peers(&topic.to_bytes().into())
            .take(REPLICATION_FACTOR.get())
            .filter_map(|peer| rpc.request(*peer.preimage(), request.as_slice()).ok())
            .collect();

        Err(Self {
            response,
            ongoing: calls,
            matched: 0,
            phantom: std::marker::PhantomData,
        })
    }

    fn resume<'a>(
        mut self,
        _: super::Scope<'a, C>,
        event: &'a Self::Event,
    ) -> super::HandlerResult<'a, Self, C> {
        crate::ensure!(let rpc::Event::Response(res) = event, self);

        log::debug!("rpc event: {:?}", res);
        match res {
            Ok((_, call, response, _)) => 'a: {
                if self.ongoing.find_and_remove(|c| c == call).is_none() {
                    break 'a;
                }

                self.matched += (self.response.as_slice() == response.as_slice()) as usize;

                if self.matched > REPLICATION_FACTOR.get() / 2 {
                    let Some(resp) = Codec::decode(&mut response.as_slice()) else {
                        return Ok(Err(ReplError::InvalidResponse));
                    };

                    return Ok(resp);
                }
            }
            Err((failed, e)) => {
                for f in failed {
                    self.ongoing.find_and_remove(|c| c == f);
                }
                log::warn!("rpc failed: {}", e);
            }
        }

        if self.ongoing.len() + self.matched < REPLICATION_FACTOR.get() / 2 {
            return Ok(Err(ReplError::NoMajority));
        }

        Err(self)
    }
}
