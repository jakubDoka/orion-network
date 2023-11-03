use std::collections::VecDeque;

use libp2p_identity::PeerId;
use libp2p_kad::{store::RecordStore, QueryId, QueryResult};

use libp2p_swarm::Swarm;

pub trait KadSearchComponent: libp2p_swarm::NetworkBehaviour {
    fn redail(&mut self, peer: PeerId);
}

pub trait KadSearchBehaviour: libp2p_swarm::NetworkBehaviour {
    type RecordStore: RecordStore + Send + 'static;
    type Component: KadSearchComponent + Send + 'static;

    fn context(
        &mut self,
    ) -> (
        &mut Self::Component,
        &mut libp2p_kad::Behaviour<Self::RecordStore>,
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
    event: &libp2p_kad::Event,
    swarm: &mut Swarm<impl KadSearchBehaviour>,
    discovery: &mut KadPeerSearch,
) -> bool {
    match discovery.try_handle_kad_event(event, swarm.behaviour_mut().context().1) {
        Some(Some(peer_id)) if swarm.is_connected(&peer_id) => {
            swarm.behaviour_mut().context().0.redail(peer_id);
        }
        Some(Some(peer_id)) => {
            swarm.dial(peer_id).unwrap();
        }
        Some(None) => {}
        None => return false,
    }

    true
}

#[macro_export]
macro_rules! impl_kad_search {
    ($ty:ty => ($component_type:ty => $component:ident)) => {
        $crate::impl_kad_search!($ty => ($crate::libp2p_kad::store::MemoryStore, $component_type => $component, kad));
    };

    ($ty:ty => ($store:ty, $onion_type:ty => $onion:ident, $kad:ident)) => {
        impl $crate::KadSearchBehaviour for $ty {
            type RecordStore = $store;
            type Component = $onion_type;

            fn context(
                &mut self,
            ) -> (
                &mut Self::Component,
                &mut $crate::libp2p_kad::Behaviour<Self::RecordStore>,
            ) {
                (&mut self.$onion, &mut self.$kad)
            }
        }
    };
}

#[derive(Default)]
pub struct KadPeerSearch {
    discovery_queries: VecDeque<(QueryId, PeerId)>,
}

impl KadPeerSearch {
    pub fn discover_peer(
        &mut self,
        peer_id: PeerId,
        kad: &mut libp2p_kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let query_id = kad.get_closest_peers(peer_id);
        self.discovery_queries.push_back((query_id, peer_id));
    }

    pub fn try_handle_kad_event(
        &mut self,
        event: &libp2p_kad::Event,
        kad: &mut libp2p_kad::Behaviour<impl RecordStore + Send + 'static>,
    ) -> Option<Option<PeerId>> {
        let libp2p_kad::Event::OutboundQueryProgressed {
            id,
            result: QueryResult::GetClosestPeers(Ok(closest_peers)),
            step,
            ..
        } = event
        else {
            return None;
        };

        let index = self.discovery_queries.iter().position(|(q, _)| q == id)?;
        let (_, target) = self.discovery_queries.swap_remove_front(index).unwrap();

        if closest_peers.peers.contains(&target) {
            if let Some(mut q) = kad.query_mut(id) {
                q.finish();
            }
            return Some(Some(target));
        }

        if !step.last {
            self.discovery_queries.push_back((*id, target));
        }

        Some(None)
    }
}

#[derive(Default)]
pub struct KadRandomPeerSearch {
    discovery_queries: VecDeque<QueryId>,
}

impl KadRandomPeerSearch {
    pub fn discover_random(
        &mut self,
        kad: &mut libp2p_kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let rpi = PeerId::random();
        let query_id = kad.get_closest_peers(rpi);
        self.discovery_queries.push_back(query_id);
    }

    pub fn try_handle_kad_event(&mut self, event: &mut libp2p_kad::Event) -> Vec<PeerId> {
        let libp2p_kad::Event::OutboundQueryProgressed {
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

        std::mem::take(&mut closest_peers.peers)
    }
}

#[derive(Default)]
pub struct KadProviderSearch {
    discovery_queries: VecDeque<(QueryId, libp2p_kad::RecordKey, Vec<PeerId>)>,
}

impl KadProviderSearch {
    pub fn discover_providers(
        &mut self,
        key: libp2p_kad::RecordKey,
        kad: &mut libp2p_kad::Behaviour<impl RecordStore + Send + 'static>,
    ) {
        let query_id = kad.get_providers(key.clone());
        self.discovery_queries.push_back((query_id, key, vec![]));
    }

    pub fn try_handle_kad_event(
        &mut self,
        event: &libp2p_kad::Event,
    ) -> Option<Option<(libp2p_kad::RecordKey, Vec<PeerId>)>> {
        let libp2p_kad::Event::OutboundQueryProgressed {
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
            libp2p_kad::GetProvidersOk::FoundProviders { providers, .. } => {
                list.extend(providers);
            }
            libp2p_kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. } => {}
        }

        if !step.last {
            self.discovery_queries.push_back((*id, key, list));
            return Some(None);
        }

        Some(Some((key, list)))
    }
}
