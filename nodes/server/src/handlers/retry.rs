//use {
//    super::{Handler, HandlerTypes},
//    chat_logic::{ExtractTopic, PossibleTopic, Protocol, REPLICATION_FACTOR},
//    component_utils::{arrayvec::ArrayVec, Codec},
//    rpc::CallId,
//};
//
//pub trait RetryPolicy: ExtractTopic {
//    fn should_retry(error: &Self::Error) -> bool;
//}
//
//pub enum Retry<H, E> {
//    Handling(H, PossibleTopic, Vec<u8>),
//    Fetching {
//        requests: ArrayVec<CallId, { REPLICATION_FACTOR.get() }>,
//        phantom: std::marker::PhantomData<fn(E)>,
//    },
//}
//
//impl<H, E> Retry<H, E> {
//    pub fn new_fetching<C: ProvideKadAndRpc>(
//        response: Vec<u8>,
//        request: Vec<u8>,
//        topic: PossibleTopic,
//        cx: &mut C,
//    ) -> Self {
//        let (kad, rpc) = cx.kad_and_rpc_mut();
//        let requests = kad
//            .get_closest_local_peers(&KBucketKey::new(topic))
//            .take(REPLICATION_FACTOR.get())
//            .filter_map(|peer| rpc.request(*peer.preimage(), request.as_slice()).ok())
//            .collect();
//
//        Self::Fetching {
//            response,
//            ongoing,
//            matched: 0,
//            phantom: std::marker::PhantomData,
//        }
//    }
//}
//
//impl<H: HandlerTypes, E> HandlerTypes for Retry<H, E> {
//    type Event = H::Event;
//    type Protocol = H::Protocol;
//}
//
//impl<C, H, E> Handler<C> for Retry<H, E>
//where
//    H: Handler<C>,
//    H::Protocol: ExtractTopic,
//{
//    fn execute<'a>(
//        scope: super::Scope<'a, C>,
//        req: <Self::Protocol as chat_logic::Protocol>::Request<'_>,
//    ) -> super::HandlerResult<'a, Self> {
//        let topic: PossibleTopic = <H::Protocol as ExtractTopic>::extract_topic(&req).into();
//
//        let response = match H::execute(scope.reborrow(), req) {
//            Ok(Ok(r)) => Ok::<_, ()>(r).to_bytes(),
//            Ok(Err(e)) => return Ok(Err(e)),
//            Err(e) => return Err(Self::Handling(e, topic, req.to_bytes())),
//        };
//
//        Err(Self::new_fetching(
//            response,
//            req.to_bytes(),
//            topic,
//            scope.cx,
//        ))
//    }
//
//    fn resume<'a>(
//        self,
//        cx: super::Scope<'a, C>,
//        enent: &'a Self::Event,
//    ) -> super::HandlerResult<'a, Self> {
//        todo!()
//    }
//}
