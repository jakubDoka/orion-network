use {
    super::Storage,
    crate::HandlerResult,
    component_utils::{Codec, Reminder},
    libp2p::{
        kad::{GetClosestPeersOk, QueryId, QueryResult},
        PeerId,
    },
    std::convert::Infallible,
};

pub struct SearchPeers {
    query: QueryId,
}

impl SearchPeers {
    pub fn new(kad: &mut libp2p::kad::Behaviour<Storage>, target: impl for<'a> Codec<'a>) -> Self {
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

impl crate::Handler for SearchPeers {
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = Infallible;
    type Request<'a> = Reminder<'a>;
    type Response<'a> = Vec<PeerId>;
    type Topic = Infallible;

    fn spawn<'a>(
        context: crate::PassedContext<'a, Self>,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Result<HandlerResult<'a, Self>, Self> {
        Err(Self::new(context.context, request))
    }

    fn try_complete(
        self,
        _: &mut Self::Context,
        _: &mut crate::EventDispatch<Self>,
        event: &<Self::Context as crate::Context>::ToSwarm,
    ) -> Result<HandlerResult<'static, Self>, Self> {
        self.try_complete(event).map(ToOwned::to_owned).map(Ok)
    }
}
