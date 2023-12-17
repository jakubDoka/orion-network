use {super::*, chat_logic::SearchPeers};

impl<C: ProvideKad + ProvidePeerId> SyncHandler<C> for SearchPeers {
    fn execute<'a>(mut cx: Scope<'a, C>, req: Self::Request<'_>) -> ProtocolResult<'a, Self> {
        let mut us = Some(KBucketKey::from(cx.peer_id()));
        let key = KBucketKey::new(req);
        let values = cx
            .kad_mut()
            .get_closest_local_peers(&key)
            .flat_map(|p| match &us {
                Some(u) if p.distance(&key) > u.distance(&key) => {
                    std::iter::once(p).chain(us.take())
                }
                _ => std::iter::once(p).chain(None),
            })
            .map(|p| *p.preimage())
            .take(REPLICATION_FACTOR.get() + 1)
            .collect::<Vec<PeerId>>();
        Ok(values)
    }
}

//pub struct SearchPeers {
//    query: QueryId,
//}
//
//impl SearchPeers {
//    pub fn new(
//        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
//        target: impl for<'a> Codec<'a>,
//    ) -> Self {
//        Self {
//            query: kad.get_closest_peers(target.to_bytes()),
//        }
//    }
//
//    pub fn try_complete(self, event: &libp2p::kad::Event) -> Result<&[PeerId], Self> {
//        crate::ensure!(let libp2p::kad::Event::OutboundQueryProgressed {
//            id,
//            result: QueryResult::GetClosestPeers(result),
//            ..
//        } = event, self);
//
//        crate::ensure!(id == &self.query, self);
//
//        let Ok(GetClosestPeersOk { peers, .. }) = result else {
//            return Ok(&[]);
//        };
//
//        Ok(peers)
//    }
//}
//
//impl HandlerTypes for SearchPeers {
//    type Event = libp2p::kad::Event;
//    type Protocol = chat_logic::SearchPeers;
//}
//
//impl<C: ProvideKad> Handler<C> for SearchPeers {
//    fn execute<'a>(
//        mut cx: Scope<'a, C>,
//        req: <Self::Protocol as Protocol>::Request<'_>,
//    ) -> HandlerResult<'a, Self> {
//        Err(Self::new(cx.kad_mut(), req))
//    }
//
//    fn resume<'a>(self, _: Scope<'a, C>, enent: &'a Self::Event) -> HandlerResult<'a, Self> {
//        Ok(Ok(self.try_complete(enent)?.into()))
//    }
//}
