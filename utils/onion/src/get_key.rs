use std::{collections::VecDeque, convert::Infallible, io, iter, mem, task::Poll};

use futures::{AsyncReadExt, Future};
use libp2p_core::{upgrade::DeniedUpgrade, OutboundUpgrade, UpgradeInfo};
use libp2p_identity::PeerId;
use libp2p_swarm::{StreamProtocol, SubstreamProtocol};
use x25519_dalek::PublicKey;

pub struct Behaviour {
    events:
        VecDeque<libp2p_swarm::ToSwarm<(PublicKey, PeerId), libp2p_swarm::THandlerInEvent<Self>>>,
}

impl Behaviour {
    pub fn new() -> Self {
        Self {
            events: Default::default(),
        }
    }
}

impl libp2p_swarm::NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;

    type ToSwarm = (PublicKey, PeerId);

    fn handle_established_inbound_connection(
        &mut self,
        _: libp2p_swarm::ConnectionId,
        _: libp2p_identity::PeerId,
        _: &libp2p_core::Multiaddr,
        _: &libp2p_core::Multiaddr,
    ) -> Result<libp2p_swarm::THandler<Self>, libp2p_swarm::ConnectionDenied> {
        Ok(Handler::default())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: libp2p_swarm::ConnectionId,
        _: libp2p_identity::PeerId,
        _: &libp2p_core::Multiaddr,
        _: libp2p_core::Endpoint,
    ) -> Result<libp2p_swarm::THandler<Self>, libp2p_swarm::ConnectionDenied> {
        Ok(Handler::default())
    }

    fn on_swarm_event(&mut self, _: libp2p_swarm::FromSwarm<Self::ConnectionHandler>) {}

    fn on_connection_handler_event(
        &mut self,
        peer_id: libp2p_identity::PeerId,
        _connection_id: libp2p_swarm::ConnectionId,
        event: libp2p_swarm::THandlerOutEvent<Self>,
    ) {
        self.events
            .push_back(libp2p_swarm::ToSwarm::GenerateEvent((event, peer_id)));
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
        _: &mut impl libp2p_swarm::PollParameters,
    ) -> std::task::Poll<libp2p_swarm::ToSwarm<Self::ToSwarm, libp2p_swarm::THandlerInEvent<Self>>>
    {
        if let Some(e) = self.events.pop_front() {
            return Poll::Ready(e);
        }

        Poll::Pending
    }
}

#[derive(Default)]
pub struct Handler {
    request_sent: bool,
    final_event: Option<PublicKey>,
}

impl libp2p_swarm::ConnectionHandler for Handler {
    type FromBehaviour = Infallible;

    type ToBehaviour = PublicKey;

    type Error = io::Error;

    type InboundProtocol = DeniedUpgrade;

    type OutboundProtocol = Upgrade;

    type InboundOpenInfo = ();

    type OutboundOpenInfo = ();

    fn listen_protocol(
        &self,
    ) -> libp2p_swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(DeniedUpgrade, ())
    }

    fn connection_keep_alive(&self) -> libp2p_swarm::KeepAlive {
        if self.request_sent {
            libp2p_swarm::KeepAlive::No
        } else {
            libp2p_swarm::KeepAlive::Yes
        }
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<
        libp2p_swarm::ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
            Self::Error,
        >,
    > {
        if !mem::replace(&mut self.request_sent, true) {
            return Poll::Ready(
                libp2p_swarm::ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(Upgrade, ()),
                },
            );
        }
        if let Some(fin) = self.final_event.take() {
            return Poll::Ready(libp2p_swarm::ConnectionHandlerEvent::NotifyBehaviour(fin));
        }
        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {}
    }

    fn on_connection_event(
        &mut self,
        event: libp2p_swarm::handler::ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            libp2p_swarm::handler::ConnectionEvent::FullyNegotiatedOutbound(o) => {
                self.final_event = Some(o.protocol)
            }
            _ => {}
        }
    }
}

pub struct Upgrade;

impl UpgradeInfo for Upgrade {
    type Info = StreamProtocol;

    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(crate::handler::KEY_SHARE_PROTOCOL)
    }
}

impl OutboundUpgrade<libp2p_swarm::Stream> for Upgrade {
    type Output = PublicKey;

    type Error = io::Error;

    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, mut socket: libp2p_swarm::Stream, _: Self::Info) -> Self::Future {
        async move {
            let mut buffer = [0; 32];
            socket.read_exact(&mut buffer).await?;
            let key = PublicKey::from(buffer);
            Ok(key)
        }
    }
}
