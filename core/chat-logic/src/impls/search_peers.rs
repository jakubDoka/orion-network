use {
    super::Storage,
    crate::HandlerResult,
    component_utils::Reminder,
    libp2p::{
        kad::{GetClosestPeersOk, QueryId, QueryResult},
        PeerId,
    },
    std::convert::Infallible,
};

pub struct SearchPeers {
    query: QueryId,
    peers: Vec<PeerId>,
}

impl crate::Handler for SearchPeers {
    type Context = libp2p::kad::Behaviour<Storage>;
    type Error = Infallible;
    type Request<'a> = Reminder<'a>;
    type Response<'a> = Vec<PeerId>;
    type Topic = Infallible;

    fn spawn(
        context: &mut Self::Context,
        request: &Self::Request<'_>,
        _: &mut crate::EventDispatch<Self>,
        _: crate::RequestMeta,
    ) -> Result<HandlerResult<'static, Self>, Self> {
        Err(Self {
            query: context.get_closest_peers(request.0.to_vec()),
            peers: Vec::new(),
        })
    }

    fn try_complete(
        mut self,
        _: &mut Self::Context,
        _: &mut crate::EventDispatch<Self>,
        event: &<Self::Context as crate::Context>::ToSwarm,
    ) -> Result<HandlerResult<'static, Self>, Self> {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(result),
            stats: _,
            step,
        } = event
        else {
            return Err(self);
        };

        crate::ensure!(id == &self.query, self);

        let Ok(GetClosestPeersOk { peers, .. }) = result else {
            return Ok(Ok(Vec::new()));
        };

        self.peers.extend(peers);

        crate::ensure!(step.last, self);

        Ok(Ok(self.peers))
    }
}
