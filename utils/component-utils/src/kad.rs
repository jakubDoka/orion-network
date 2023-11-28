use {
    crate::LinearMap,
    libp2p::{
        identity::PeerId,
        kad::{store::RecordStore, QueryId, QueryResult},
        swarm::Swarm,
    },
    std::collections::VecDeque,
};

pub trait KadSearchComponent: libp2p::swarm::NetworkBehaviour {
    fn redail(&mut self, peer: PeerId);
    fn mark_failed(&mut self, peer: PeerId);
}

pub trait KadSearchBehaviour: libp2p::swarm::NetworkBehaviour {
    type RecordStore: RecordStore + Send + 'static;
    type Component: KadSearchComponent + Send + 'static;

    fn context(
        &mut self,
    ) -> (
        &mut Self::Component,
        &mut libp2p::kad::Behaviour<Self::RecordStore>,
    );
}

pub fn handle_conn_request(
    to: PeerId,
    swarm: &mut Swarm<impl KadSearchBehaviour>,
    discovery: &mut KadPeerSearch,
) {
    if swarm.is_connected(&to) {
        swarm.behaviour_mut().context().0.redail(to);
    } else {
        discovery.discover_peer(to, swarm.behaviour_mut().context().1);
    }
}

pub fn try_handle_conn_response(
    event: &libp2p::kad::Event,
    swarm: &mut Swarm<impl KadSearchBehaviour>,
    discovery: &mut KadPeerSearch,
) -> bool {
    match discovery.try_handle_kad_event(event, swarm.behaviour_mut().context().1) {
        KadSearchResult::Discovered(peer_id) if swarm.is_connected(&peer_id) => {
            swarm.behaviour_mut().context().0.redail(peer_id);
        }
        KadSearchResult::Discovered(peer_id) => {
            swarm.dial(peer_id).unwrap();
        }
        KadSearchResult::Pending => {}
        KadSearchResult::Failed(peer_id) => {
            swarm.behaviour_mut().context().0.mark_failed(peer_id);
        }
        KadSearchResult::Ignored => return false,
    }

    true
}

#[macro_export]
macro_rules! impl_kad_search {
    ($ty:ty => ($component_type:ty => $component:ident)) => {
        $crate::impl_kad_search!($ty => ($crate::libp2p::kad::store::MemoryStore, $component_type => $component, kad));
    };

    ($ty:ty => ($store:ty, $onion_type:ty => $onion:ident, $kad:ident)) => {
        impl $crate::KadSearchBehaviour for $ty {
            type RecordStore = $store;
            type Component = $onion_type;

            fn context(
                &mut self,
            ) -> (
                &mut Self::Component,
                &mut $crate::libp2p::kad::Behaviour<Self::RecordStore>,
            ) {
                (&mut self.$onion, &mut self.$kad)
            }
        }
    };
}

#[derive(Default)]
pub struct KadPeerSearch {
    discovery_queries: LinearMap<QueryId, PeerId>,
}

pub enum KadSearchResult {
    Ignored,
    Discovered(PeerId),
    Failed(PeerId),
    Pending,
}

impl KadPeerSearch {
    pub fn discover_peer(
        &mut self,
        peer_id: PeerId,
        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let query_id = kad.get_closest_peers(peer_id);
        self.discovery_queries.insert(query_id, peer_id);
    }

    pub fn try_handle_kad_event(
        &mut self,
        event: &libp2p::kad::Event,
        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
    ) -> KadSearchResult {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(Ok(closest_peers)),
            step,
            ..
        } = event
        else {
            return KadSearchResult::Ignored;
        };

        let Some(target) = self.discovery_queries.remove(id) else {
            return KadSearchResult::Ignored;
        };

        if closest_peers.peers.contains(&target) {
            if let Some(mut q) = kad.query_mut(id) {
                q.finish();
            }
            return KadSearchResult::Discovered(target);
        }

        if !step.last {
            self.discovery_queries.insert(*id, target);
            return KadSearchResult::Pending;
        }

        KadSearchResult::Failed(target)
    }
}

#[derive(Default)]
pub struct KadRandomPeerSearch {
    discovery_queries: VecDeque<QueryId>,
}

impl KadRandomPeerSearch {
    pub fn discover_random(
        &mut self,
        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let rpi = PeerId::random();
        let query_id = kad.get_closest_peers(rpi);
        self.discovery_queries.push_back(query_id);
    }

    pub fn try_handle_kad_event(&mut self, event: &mut libp2p::kad::Event) -> Vec<PeerId> {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(Ok(closest_peers)),
            ..
        } = event
        else {
            return vec![];
        };

        let Some(index) = self.discovery_queries.iter().position(|q| q == id) else {
            return vec![];
        };
        self.discovery_queries.swap_remove_front(index).unwrap();

        core::mem::take(&mut closest_peers.peers)
    }
}

#[derive(Default)]
pub struct KadProviderSearch {
    discovery_queries: VecDeque<(QueryId, libp2p::kad::RecordKey, Vec<PeerId>)>,
}

impl KadProviderSearch {
    pub fn discover_providers(
        &mut self,
        key: libp2p::kad::RecordKey,
        kad: &mut libp2p::kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let query_id = kad.get_providers(key.clone());
        self.discovery_queries.push_back((query_id, key, vec![]));
    }

    pub fn try_handle_kad_event(
        &mut self,
        event: &libp2p::kad::Event,
    ) -> Option<Option<(libp2p::kad::RecordKey, Vec<PeerId>)>> {
        let libp2p::kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetProviders(Ok(providers)),
            step,
            ..
        } = event
        else {
            return None;
        };

        let index = self.discovery_queries.iter().position(|(q, ..)| q == id)?;
        let (_, key, mut list) = self.discovery_queries.swap_remove_front(index).unwrap();
        match providers {
            libp2p::kad::GetProvidersOk::FoundProviders { providers, .. } => {
                list.extend(providers);
            }
            libp2p::kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. } => {}
        }

        if !step.last {
            self.discovery_queries.push_back((*id, key, list));
            return Some(None);
        }

        Some(Some((key, list)))
    }
}
