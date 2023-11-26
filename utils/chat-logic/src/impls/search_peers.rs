use {
    component_utils::Codec,
    libp2p::{
        kad::{GetClosestPeersOk, QueryId, QueryResult},
        PeerId,
    },
};

use super::Identity;

use super::{ChatName, Storage};

pub struct SearchPeers<T> {
    query: QueryId,
    peers: Vec<PeerId>,
    _marker: std::marker::PhantomData<T>,
}

pub trait Searcher {
    type Key<'a>: Codec<'a>;
}

pub struct ProfileQ;
impl Searcher for ProfileQ {
    type Key<'a> = Identity;
}

pub struct ChatQ;
impl Searcher for ChatQ {
    type Key<'a> = ChatName;
}

impl<T: Searcher> crate::AsyncHandler for SearchPeers<T> {
    type Request<'a> = T::Key<'a>;
    type Response<'a> = Vec<PeerId>;
    type Context = libp2p::kad::Behaviour<Storage>;

    fn spawn(
        context: &mut Self::Context,
        request: Self::Request<'_>,
        _: crate::RequestMeta,
    ) -> Result<Self::Response<'static>, Self> {
        Err(Self {
            query: context.get_closest_peers(request.to_bytes()),
            peers: Vec::new(),
            _marker: std::marker::PhantomData,
        })
    }

    fn try_complete(
        mut self,
        _: &mut Self::Context,
        event: &<Self::Context as crate::MinimalNetworkBehaviour>::ToSwarm,
    ) -> Result<Self::Response<'static>, Self> {
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
            return Ok(Vec::new());
        };

        self.peers.extend(peers);

        crate::ensure!(step.last, self);

        Ok(self.peers)
    }
}
