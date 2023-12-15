use {
    libp2p::{
        core::{Endpoint, Multiaddr},
        identity::PeerId,
        multiaddr::Protocol,
        swarm::{
            dummy, CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour,
            THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
        },
    },
    std::{
        collections::{HashSet, VecDeque},
        convert::Infallible,
        fmt,
        task::{Context, Poll, Waker},
    },
};

#[derive(Default, Debug)]
pub struct Behaviour {
    peers: HashSet<PeerId>,
    free_port: u16,
    close_connections: VecDeque<PeerId>,
    waker: Option<Waker>,
}

impl Behaviour {
    pub fn new(free_port: u16) -> Self {
        Self {
            free_port,
            ..Default::default()
        }
    }

    pub fn allow_peer(&mut self, peer: PeerId) {
        self.peers.insert(peer);
        if let Some(waker) = self.waker.take() {
            waker.wake()
        }
    }

    pub fn disallow_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        self.close_connections.push_back(peer);
        if let Some(waker) = self.waker.take() {
            waker.wake()
        }
    }

    pub fn is_allowed(&self, peer: &PeerId) -> bool {
        self.peers.contains(peer)
    }

    fn enforce_inbound(&self, peer: &PeerId, addr: &Multiaddr) -> Result<(), ConnectionDenied> {
        if !self.peers.contains(peer)
            && !addr
                .iter()
                .any(|addr| matches!(addr, Protocol::Tcp(port) if port == self.free_port))
        {
            log::warn!("peer {} is not in the allow list, nor is a client", peer);
            return Err(ConnectionDenied::new(NotAllowed { peer: *peer }));
        }

        Ok(())
    }

    fn enforce_outbound(&self, peer: &PeerId) -> Result<(), ConnectionDenied> {
        if !self.peers.contains(peer) {
            log::warn!("peer {} is not in the allow list", peer);
            return Err(ConnectionDenied::new(NotAllowed { peer: *peer }));
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct NotAllowed {
    peer: PeerId,
}

impl fmt::Display for NotAllowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peer {} is not in the allow list", self.peer)
    }
}

impl std::error::Error for NotAllowed {}

#[derive(Debug)]
pub struct Blocked {
    peer: PeerId,
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "peer {} is in the block list", self.peer)
    }
}

impl std::error::Error for Blocked {}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        local: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce_inbound(&peer, local)?;

        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer {
            self.enforce_outbound(&peer)?;
        }

        Ok(vec![])
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce_outbound(&peer)?;

        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, _event: FromSwarm) {}

    fn on_connection_handler_event(
        &mut self,
        _id: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(peer) = self.close_connections.pop_front() {
            return Poll::Ready(ToSwarm::CloseConnection {
                peer_id: peer,
                connection: CloseConnection::All,
            });
        }

        self.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
