#![feature(extract_if)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
#![feature(macro_metavar_expr)]

use {
    component_utils::{Codec, CodecExt, FindAndRemove, LinearMap},
    libp2p::{
        core::UpgradeInfo,
        futures::AsyncWriteExt,
        swarm::{dial_opts::DialOpts, ConnectionHandler, ConnectionId, NetworkBehaviour},
        InboundUpgrade, OutboundUpgrade, PeerId, StreamProtocol,
    },
    std::{future::Future, io, sync::Arc, task::Poll},
};

component_utils::decl_stream_protocol!(PROTOCOL_NAME = "rpc");

#[derive(Default)]
pub struct Behaviour {
    peers: LinearMap<PeerId, ConnectionId>,
    pending_requests: Vec<(PeerId, Packet)>,
    sending_requests: Vec<(PeerId, ConnectionId, Packet)>,
    pending_dials: Vec<PeerId>,
    events: Vec<Event>,
}

impl Behaviour {
    pub fn request(&mut self, peer: PeerId, packet: impl Into<Arc<[u8]>>) -> CallId {
        let call = CallId::new();
        let packet = Packet {
            call,
            payload: packet.into(),
        };

        if let Some(&connection_id) = self.peers.get(&peer) {
            self.sending_requests.push((peer, connection_id, packet));
        } else {
            self.pending_requests.push((peer, packet));
            self.pending_dials.push(peer);
        }
        call
    }

    fn handle_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        if self.peers.insert(peer, connection_id).is_some() {
            log::warn!("duplicate connection for peer {}", peer);
            return Ok(Handler::default());
        }

        for (peer, packet) in self.pending_requests.extract_if(|(p, ..)| *p == peer) {
            self.sending_requests.push((peer, connection_id, packet));
        }

        Ok(Handler::default())
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _: &libp2p::Multiaddr,
        _: &libp2p::Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.handle_connection(connection_id, peer)
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _: &libp2p::Multiaddr,
        _: libp2p::core::Endpoint,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.handle_connection(connection_id, peer)
    }

    fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
        if let libp2p::swarm::FromSwarm::ConnectionClosed(c) = event {
            for (peer, _, packet) in self
                .sending_requests
                .extract_if(|(_, cid, ..)| *cid == c.connection_id)
            {
                self.pending_requests.push((peer, packet));
            }
            if self.pending_requests.iter().any(|(p, _)| *p == c.peer_id) {
                self.pending_dials.push(c.peer_id);
            }
            self.peers.remove(&c.peer_id);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        let ev = match event {
            ToBehaviour::Request(packet, stream) => {
                self.sending_requests
                    .find_and_remove(|(.., p)| p.call == packet.call);
                Event::Request(packet, peer_id, stream)
            }
            ToBehaviour::Response(packet) => Event::Response(packet),
            ToBehaviour::Failed(call) => {
                if let Some((.., p)) = self
                    .sending_requests
                    .find_and_remove(|(.., p)| p.call == call)
                {
                    Event::Failed(p)
                } else {
                    return;
                }
            }
        };

        self.events.push(ev);
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
        if let Some((peer, connection_id, packet)) = self.sending_requests.pop() {
            return Poll::Ready(libp2p::swarm::ToSwarm::NotifyHandler {
                peer_id: peer,
                handler: libp2p::swarm::NotifyHandler::One(connection_id),
                event: packet,
            });
        }

        if let Some(peer) = self.pending_dials.pop() {
            return Poll::Ready(libp2p::swarm::ToSwarm::Dial {
                opts: DialOpts::peer_id(peer)
                    .condition(libp2p::swarm::dial_opts::PeerCondition::NotDialing)
                    .build(),
            });
        }

        if let Some(event) = self.events.pop() {
            return Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(event));
        }

        Poll::Pending
    }
}

component_utils::gen_unique_id!(CallId);

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Request(Packet, PeerId, libp2p::Stream),
    Response(Packet),
    Failed(Packet),
}

#[derive(Debug)]
pub enum ToBehaviour {
    Request(Packet, libp2p::Stream),
    Response(Packet),
    Failed(CallId),
}

component_utils::protocol! {'a:
    #[derive(Debug, Clone)]
    struct Packet {
        call: CallId,
        payload: Arc<[u8]>,
    }
}

#[derive(Default)]
pub struct Handler {
    to_behaviour: Vec<ToBehaviour>,
    to_request: Vec<Packet>,
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Packet;
    type InboundOpenInfo = ();
    type InboundProtocol = InboundProtocol;
    type OutboundOpenInfo = CallId;
    type OutboundProtocol = OutboundProtocol;
    type ToBehaviour = ToBehaviour;

    fn listen_protocol(
        &self,
    ) -> libp2p::swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        libp2p::swarm::SubstreamProtocol::new(InboundProtocol, ())
    }

    fn poll(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> Poll<
        libp2p::swarm::ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
        >,
    > {
        if let Some(event) = self.to_behaviour.pop() {
            return Poll::Ready(libp2p::swarm::ConnectionHandlerEvent::NotifyBehaviour(
                event,
            ));
        }

        if let Some(packet) = self.to_request.pop() {
            let info = packet.call;
            return Poll::Ready(
                libp2p::swarm::ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: libp2p::swarm::SubstreamProtocol::new(OutboundProtocol(packet), info),
                },
            );
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        self.to_request.push(event);
    }

    fn on_connection_event(
        &mut self,
        event: libp2p::swarm::handler::ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        use libp2p::swarm::handler::ConnectionEvent as E;
        let ev = match event {
            E::FullyNegotiatedInbound(i) => ToBehaviour::Request(i.protocol.0, i.protocol.1),
            E::FullyNegotiatedOutbound(o) => ToBehaviour::Response(o.protocol),
            E::DialUpgradeError(e) => ToBehaviour::Failed(e.info),
            _ => return,
        };
        self.to_behaviour.push(ev);
    }
}

pub struct InboundProtocol;

impl UpgradeInfo for InboundProtocol {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl InboundUpgrade<libp2p::Stream> for InboundProtocol {
    type Error = io::Error;
    type Output = (Packet, libp2p::Stream);

    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, mut socket: libp2p::Stream, _: Self::Info) -> Self::Future {
        async move {
            let packet = Packet::from_stream(&mut socket).await?;
            Ok((packet, socket))
        }
    }
}

pub struct OutboundProtocol(Packet);

impl UpgradeInfo for OutboundProtocol {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl OutboundUpgrade<libp2p::Stream> for OutboundProtocol {
    type Error = io::Error;
    type Output = Packet;

    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, mut socket: libp2p::Stream, _: Self::Info) -> Self::Future {
        async move {
            socket.write_all(&self.0.to_bytes()).await?;
            Packet::from_stream(&mut socket).await
        }
    }
}

pub type RespondFuture = impl Future<Output = io::Result<()>> + Send;
pub fn respond(packet: Packet, mut stream: libp2p::Stream) -> RespondFuture {
    async move { stream.write_all(&packet.to_bytes()).await }
}

#[cfg(test)]
mod test {
    use {
        super::*,
        libp2p::{
            futures::{stream::FuturesUnordered, StreamExt},
            multiaddr::Protocol,
            Multiaddr, Transport,
        },
        std::net::Ipv4Addr,
    };

    #[derive(NetworkBehaviour)]
    struct TestBehatiour {
        rpc: Behaviour,
        kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    }

    #[tokio::test]
    async fn test_random_rpc_calls() {
        env_logger::init();

        let pks = (0..2)
            .map(|_| libp2p::identity::Keypair::generate_ed25519())
            .collect::<Vec<_>>();
        let peer_ids = pks
            .iter()
            .map(|kp| kp.public().to_peer_id())
            .collect::<Vec<_>>();
        let servers = pks.into_iter().enumerate().map(|(i, kp)| {
            let peer_id = kp.public().to_peer_id();
            let beh = TestBehatiour {
                rpc: Behaviour::default(),
                kad: libp2p::kad::Behaviour::new(
                    peer_id,
                    libp2p::kad::store::MemoryStore::new(peer_id),
                ),
            };
            let transport = libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default())
                .upgrade(libp2p::core::upgrade::Version::V1)
                .authenticate(libp2p::noise::Config::new(&kp).unwrap())
                .multiplex(libp2p::yamux::Config::default())
                .boxed();
            let mut swarm = libp2p::Swarm::new(
                transport,
                beh,
                kp.public().to_peer_id(),
                libp2p::swarm::Config::with_tokio_executor(),
            );

            swarm
                .listen_on(
                    Multiaddr::empty()
                        .with(Protocol::Ip4(Ipv4Addr::LOCALHOST))
                        .with(Protocol::Tcp(3000 + i as u16)),
                )
                .unwrap();

            for (j, peer_id) in peer_ids.iter().enumerate() {
                if i == j {
                    continue;
                }
                swarm.behaviour_mut().kad.add_address(
                    peer_id,
                    Multiaddr::empty()
                        .with(Protocol::Ip4(Ipv4Addr::LOCALHOST))
                        .with(Protocol::Tcp(3000 + j as u16)),
                );
            }

            swarm
        });

        async fn run_server(mut swarm: libp2p::Swarm<TestBehatiour>, mut all_peers: Vec<PeerId>) {
            all_peers.retain(|p| p != swarm.local_peer_id());
            let max_pending_requests = 10;
            let request_goal = 1000;
            let mut pending_request_count = 0;
            let mut total_requests = 0;
            let mut iteration = 0;
            let mut responses = FuturesUnordered::new();
            loop {
                if max_pending_requests > pending_request_count && request_goal > total_requests {
                    let peer_id = all_peers[iteration % all_peers.len()];
                    swarm.behaviour_mut().rpc.request(peer_id, []);
                    pending_request_count += 1;
                    total_requests += 1;
                } else if responses.is_empty()
                    && pending_request_count == 0
                    && total_requests == request_goal
                {
                    break;
                }

                let e = libp2p::futures::select! {
                    e = swarm.select_next_some() => e,
                    _ = responses.select_next_some() => {
                        continue;
                    },
                };

                match e {
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Request(packet, _, stream),
                    )) => {
                        responses.push(respond(packet, stream));
                    }
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Response(_),
                    )) => {
                        pending_request_count -= 1;
                    }
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Failed(_),
                    )) => {
                        panic!("failed request");
                    }
                    e => {
                        log::info!("{:?}", e);
                    }
                }

                iteration += 1;
            }
        }

        servers
            .map(|s| run_server(s, peer_ids.clone()))
            .collect::<FuturesUnordered<_>>()
            .for_each(|_| async {})
            .await;
    }
}
