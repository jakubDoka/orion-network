#![feature(trait_alias)]
#![feature(let_chains)]
use {
    libp2p::{
        identity::{ed25519, PublicKey},
        multihash::Multihash,
        swarm::NetworkBehaviour,
        Multiaddr, PeerId,
    },
    primitive_types::U256,
    std::{convert::Infallible, iter},
};

pub trait Filter = FnMut(
    &mut RoutingTable,
    PeerId,
    &Multiaddr,
    &Multiaddr,
) -> Result<(), libp2p::swarm::ConnectionDenied>;

pub struct Behaviour {
    pub table: RoutingTable,
    filter: Box<dyn Filter>,
}

impl Behaviour {
    pub fn new<F: Filter + 'static>(filter: F) -> Self {
        Self {
            table: RoutingTable::default(),
            filter: Box::new(filter),
        }
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = libp2p::swarm::dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: libp2p::swarm::ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        (self.filter)(&mut self.table, peer, local_addr, remote_addr)?;
        Ok(libp2p::swarm::dummy::ConnectionHandler)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: libp2p::swarm::ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(libp2p::swarm::dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _connection_id: libp2p::swarm::ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        _effective_role: libp2p::core::Endpoint,
    ) -> Result<Vec<Multiaddr>, libp2p::swarm::ConnectionDenied> {
        if addresses.is_empty()
            && let Some(peer) = maybe_peer
            && let Some(addr) = self.table.get(peer)
        {
            return Ok(vec![addr.clone()]);
        }

        Ok(vec![])
    }

    fn on_swarm_event(&mut self, _: libp2p::swarm::FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _peer_id: PeerId,
        _connection_id: libp2p::swarm::ConnectionId,
        _event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        match _event {}
    }

    fn poll(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
    {
        std::task::Poll::Pending
    }
}

#[derive(Default)]
pub struct RoutingTable {
    // sorted vec is perfect since we almost never insert new entries
    routes: Vec<Route>,
}

impl RoutingTable {
    pub fn iter(&self) -> impl Iterator<Item = &Route> {
        self.routes.iter()
    }

    pub fn bulk_insert(&mut self, routes: impl IntoIterator<Item = Route>) {
        assert!(self.routes.is_empty());
        self.routes.extend(routes);
        self.routes.sort_by_key(|r| r.id);
    }

    pub fn insert(&mut self, route: Route) {
        match self.routes.binary_search_by_key(&route.id, |r| r.id) {
            Ok(i) => self.routes[i] = route,
            Err(i) => self.routes.insert(i, route),
        }
    }

    pub fn remove(&mut self, id: PeerId) -> Option<Route> {
        let id = try_peer_id_to_ed(id)?;
        let index = self
            .routes
            .binary_search_by_key(&id.into(), |r| r.id)
            .ok()?;
        Some(self.routes.remove(index))
    }

    pub fn get(&self, id: PeerId) -> Option<&Multiaddr> {
        let id = try_peer_id_to_ed(id)?;
        let id: U256 = id.into();
        let index = self.routes.binary_search_by_key(&id, |r| r.id).ok()?;
        Some(&self.routes[index].addr)
    }

    pub fn closest(&self, data: &[u8]) -> impl Iterator<Item = &Route> + '_ {
        let hash = blake3::hash(data);
        let id = U256::from(hash.as_bytes());
        let index = self
            .routes
            .binary_search_by_key(&id, |r| r.id)
            .unwrap_or_else(|i| i);

        let mut left = self.routes[..index].iter().rev();
        let mut right = self.routes[index..].iter();
        let mut left_peek = left.next();
        let mut right_peek = right.next();

        iter::from_fn(move || {
            let (left_route, right_route) = match (left_peek, right_peek) {
                (Some(left), Some(right)) => (left, right),
                // we do not peek anymore since this must be the last one
                (Some(either), None) | (None, Some(either)) => return Some(either),
                (None, None) => return None,
            };

            let dist_left = id.abs_diff(left_route.id);
            let dist_right = right_route.id.abs_diff(id);

            if dist_left < dist_right {
                left_peek = left.next().or_else(|| right.next_back());
                Some(left_route)
            } else {
                right_peek = right.next().or_else(|| left.next_back());
                Some(right_route)
            }
        })
    }
}

pub fn try_peer_id_to_ed(id: PeerId) -> Option<[u8; 32]> {
    let multihash: &Multihash<64> = id.as_ref();
    let bytes = multihash.digest();
    bytes[bytes.len() - 32..].try_into().ok()
}

pub struct Route {
    id: U256,
    pub addr: Multiaddr,
}

impl Route {
    pub fn new(id: ed25519::PublicKey, addr: Multiaddr) -> Self {
        let id: U256 = id.to_bytes().into();
        Self { id, addr }
    }

    pub fn peer_id(&self) -> PeerId {
        let bytes: [u8; 32] = self.id.into();
        let key = ed25519::PublicKey::try_from_bytes(&bytes).expect("id to always be valid ed key");
        let key = PublicKey::from(key);
        PeerId::from(key)
    }
}
