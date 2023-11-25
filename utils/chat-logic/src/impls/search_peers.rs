use component_utils::Codec;
use crypto::sign;
use libp2p::{
    kad::{GetClosestPeersOk, QueryId, QueryResult},
    PeerId,
};

use super::{ChatName, KadStorage};

pub struct SearchPeers<T> {
    query: QueryId,
    peers: Vec<PeerId>,
    _marker: std::marker::PhantomData<T>,
}

pub trait Searcher {
    const PREFIX: u8;
    type Key<'a>: Codec<'a>;
}

pub struct Profile;
impl Searcher for Profile {
    const PREFIX: u8 = 0;
    type Key<'a> = crypto::Hash<sign::PublicKey>;
}

pub struct Chat;
impl Searcher for Chat {
    const PREFIX: u8 = 1;
    type Key<'a> = ChatName;
}

impl<T: Searcher> crate::Handler for SearchPeers<T> {
    const PREFIX: u8 = T::PREFIX;

    type Request<'a> = T::Key<'a>;
    type Response<'a> = Vec<PeerId>;
    type Context = libp2p::kad::Behaviour<KadStorage>;
    type EventResult = ();

    fn spawn(
        context: &mut Self::Context,
        request: Self::Request<'_>,
        _: crate::OutPacket<'_, Self>,
    ) -> Option<Self> {
        Some(Self {
            query: context.get_closest_peers(request.to_bytes()),
            peers: Vec::new(),
            _marker: std::marker::PhantomData,
        })
    }

    fn try_handle_event(
        &mut self,
        _context: &mut Self::Context,
        event: &<Self::Context as crate::MinimalNetworkBehaviour>::ToSwarm,
    ) -> Option<Self::EventResult> {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(result),
            stats: _,
            step,
        } = event
        else {
            return None;
        };

        if id != &self.query {
            return None;
        }

        let Ok(GetClosestPeersOk { peers, .. }) = result else {
            return Some(());
        };

        self.peers.extend(peers);

        if !step.last {
            return None;
        }

        Some(())
    }

    fn try_complete(
        self,
        _: Self::EventResult,
        _: &mut Self::Context,
        resp_buffer: crate::OutPacket<'_, Self>,
    ) -> Option<Self> {
        resp_buffer.push(&self.peers);
        None
    }
}
