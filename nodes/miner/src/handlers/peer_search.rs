use {
    super::*,
    crate::HandlerResult,
    component_utils::Codec,
    libp2p::{
        kad::{store::RecordStore, GetClosestPeersOk, QueryId, QueryResult},
        PeerId,
    },
};

pub struct SearchPeers {
    query: QueryId,
}

impl SearchPeers {
    pub fn new(
        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
        target: impl for<'a> Codec<'a>,
    ) -> Self {
        Self {
            query: kad.get_closest_peers(target.to_bytes()),
        }
    }

    pub fn try_complete(self, event: &libp2p::kad::Event) -> Result<&[PeerId], Self> {
        crate::ensure!(let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(result),
            ..
        } = event, self);

        crate::ensure!(id == &self.query, self);

        let Ok(GetClosestPeersOk { peers, .. }) = result else {
            return Ok(&[]);
        };

        Ok(peers)
    }
}

impl HandlerTypes for SearchPeers {
    type Event = libp2p::kad::Event;
    type Protocol = chat_logic::SearchPeers;
}

impl<C: ProvideKad> Handler<C> for SearchPeers {
    fn execute<'a>(
        mut cx: Scope<'a, C>,
        req: <Self::Protocol as Protocol>::Request<'_>,
    ) -> HandlerResult<'a, Self> {
        Err(Self::new(cx.kad_mut(), req))
    }

    fn resume<'a>(self, _: Scope<'a, C>, enent: &'a Self::Event) -> HandlerResult<'a, Self> {
        Ok(Ok(self.try_complete(enent)?.into()))
    }
}
