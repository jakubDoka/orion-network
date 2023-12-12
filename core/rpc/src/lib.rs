#![feature(extract_if)]
#![feature(type_alias_impl_trait)]
#![feature(impl_trait_in_assoc_type)]
#![feature(macro_metavar_expr)]

use {
    component_utils::{Codec, FindAndRemove, PacketReader, PacketWriter, Reminder},
    libp2p::{
        core::UpgradeInfo,
        futures::{stream::SelectAll, AsyncWriteExt, StreamExt},
        swarm::{
            dial_opts::DialOpts, ConnectionHandler, ConnectionId, DialFailure, NetworkBehaviour,
            StreamUpgradeError, SubstreamProtocol,
        },
        InboundUpgrade, OutboundUpgrade, PeerId, StreamProtocol,
    },
    std::{convert::Infallible, io, ops::DerefMut, task::Poll, time::Duration},
};

component_utils::decl_stream_protocol!(PROTOCOL_NAME = "rpc");

pub struct Stream {
    writer: PacketWriter,
    reader: PacketReader,
    inner: Option<libp2p::Stream>,
    peer: PeerId,
    last_packet: std::time::Instant,
}

type IsRequest = bool;

impl libp2p::futures::Stream for Stream {
    type Item = (PeerId, io::Result<(CallId, Vec<u8>, IsRequest)>);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.deref_mut();
        let Some(stream) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };

        let f = this.writer.poll(cx, stream);
        if let Poll::Ready(Err(e)) = f {
            this.inner.take();
            return Poll::Ready(Some((this.peer, Err(e))));
        }

        let read = match libp2p::futures::ready!(this.reader.poll_packet(cx, stream)) {
            Ok(r) => r,
            Err(err) => {
                this.inner.take();
                return Poll::Ready(Some((this.peer, Err(err))));
            }
        };

        let Some((call, is_request, Reminder(payload))) = <_>::decode(&mut &*read) else {
            this.inner.take();
            log::warn!("invalid packet from {}", this.peer);
            return Poll::Ready(Some((this.peer, Err(io::ErrorKind::InvalidData.into()))));
        };

        this.last_packet = std::time::Instant::now();
        Poll::Ready(Some((this.peer, Ok((call, payload.to_vec(), is_request)))))
    }
}

impl Stream {
    pub fn write(&mut self, call: CallId, payload: &[u8], is_request: bool) -> io::Result<()> {
        self.last_packet = std::time::Instant::now();
        self.writer
            .write(&(call, is_request, Reminder(payload)))
            .ok_or(io::ErrorKind::OutOfMemory)?;
        Ok(())
    }

    pub fn close(&mut self) {
        self.inner.take();
    }

    fn new(peer: PeerId, stream: libp2p::Stream, buffer_size: usize) -> Self {
        Self {
            writer: PacketWriter::new(buffer_size),
            reader: PacketReader::default(),
            inner: Some(stream),
            peer,
            last_packet: std::time::Instant::now(),
        }
    }
}

#[derive(Default)]
pub struct Behaviour {
    config: Config,
    streams: SelectAll<Stream>,
    pending_requests: Vec<(PeerId, CallId, Vec<u8>, std::time::Instant)>,
    ongoing_requests: Vec<(CallId, PeerId, std::time::Instant)>,
    pending_repsonses: Vec<(PeerId, CallId, Vec<u8>)>,
    pending_dials: Vec<PeerId>,
    ongoing_dials: Vec<PeerId>,
    events: Vec<Event>,
}

impl Behaviour {
    pub fn request(&mut self, peer: PeerId, packet: impl AsRef<[u8]> + Into<Vec<u8>>) -> CallId {
        let call = CallId::new();
        if let Some(stream) = self.streams.iter_mut().find(|s| s.peer == peer) {
            self.ongoing_requests
                .push((call, peer, std::time::Instant::now()));
            stream.write(call, packet.as_ref(), true);
        } else {
            self.pending_requests
                .push((peer, call, packet.into(), std::time::Instant::now()));
            if !self.ongoing_dials.contains(&peer) && !self.pending_dials.contains(&peer) {
                self.pending_dials.push(peer);
            }
        }
        call
    }

    pub fn respond(
        &mut self,
        peer: PeerId,
        call: CallId,
        payload: impl AsRef<[u8]> + Into<Vec<u8>>,
    ) {
        if let Some(stream) = self.streams.iter_mut().find(|s| peer == s.peer) {
            stream.write(call, payload.as_ref(), false);
        } else {
            self.pending_repsonses.push((peer, call, payload.into()));
            if !self.ongoing_dials.contains(&peer) && !self.pending_dials.contains(&peer) {
                self.pending_dials.push(peer);
            }
        }
    }

    fn handle_connection(
        &mut self,
        peer: PeerId,
        listening: bool,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        let duplicate = self.streams.iter().any(|s| s.peer == peer);
        if duplicate {
            log::warn!("duplicate connection for peer {}", peer);
        }

        let dialing = self.ongoing_dials.contains(&peer)
            || self.pending_dials.find_and_remove_value(&peer).is_some();

        Ok(Handler {
            requested: listening || !dialing || duplicate,
            stream: None,
            waker: None,
        })
    }

    fn clean_failed_requests(&mut self, failed: PeerId, error: StreamUpgradeError<Infallible>) {
        self.pending_repsonses.retain(|(p, ..)| *p != failed);
        let failed = self
            .ongoing_requests
            .extract_if(|(_, p, ..)| *p == failed)
            .map(|(c, ..)| c)
            .chain(
                self.pending_requests
                    .extract_if(|(p, ..)| *p == failed)
                    .map(|(_, c, ..)| c),
            )
            .collect::<Vec<_>>();
        if !failed.is_empty() {
            self.events.push(Event::Response(Err((failed, error))));
        }
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &libp2p::Multiaddr,
        _: &libp2p::Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.handle_connection(peer, true)
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &libp2p::Multiaddr,
        _: libp2p::core::Endpoint,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        self.handle_connection(peer, false)
    }

    fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
        if let libp2p::swarm::FromSwarm::DialFailure(DialFailure {
            peer_id: Some(peer_id),
            error,
            ..
        }) = event
        {
            self.ongoing_dials.find_and_remove_value(&peer_id);
            self.clean_failed_requests(
                peer_id,
                StreamUpgradeError::Io(io::Error::other(error.to_string())),
            );
        }

        if let libp2p::swarm::FromSwarm::ConnectionClosed(c) = event {
            if self.pending_requests.iter().any(|(p, ..)| *p == c.peer_id)
                || self.pending_repsonses.iter().any(|(p, ..)| *p == c.peer_id)
            {
                self.pending_dials.push(c.peer_id);
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        match event {
            Ok(stream) => {
                self.ongoing_dials.find_and_remove_value(&peer_id);
                let mut stream = Stream::new(peer_id, stream, self.config.buffer_size);
                for (peer, call, packet, rt) in
                    self.pending_requests.extract_if(|(p, ..)| *p == peer_id)
                {
                    stream.write(call, &packet, true);
                    self.ongoing_requests.push((call, peer, rt));
                }
                for (_, call, packet) in self.pending_repsonses.extract_if(|(p, ..)| *p == peer_id)
                {
                    stream.write(call, &packet, false);
                }
                self.streams.push(stream);
            }
            Err(e) => self.clean_failed_requests(peer_id, e),
        }
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
        while let Poll::Ready(Some((pid, res))) = self.streams.poll_next_unpin(cx) {
            match res {
                Ok((cid, content, false)) => {
                    let Some((_, peer, time)) =
                        self.ongoing_requests.find_and_remove(|(c, ..)| *c == cid)
                    else {
                        log::warn!("unexpected response {:?}", cid);
                        continue;
                    };
                    if pid != peer {
                        log::warn!("unexpected response {:?} from {:?}", cid, peer);
                        continue;
                    }
                    return Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(Event::Response(
                        Ok((pid, cid, content, time.elapsed())),
                    )));
                }
                Ok((cid, content, true)) => {
                    return Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(Event::Request(
                        pid, cid, content,
                    )));
                }
                Err(e) => self.clean_failed_requests(pid, StreamUpgradeError::Io(e)),
            }
        }

        if let Some(peer) = self.pending_dials.pop() {
            self.ongoing_dials.push(peer);
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

component_utils::gen_config! {
    ;;
    max_cached_connections: usize = 10,
    buffer_size: usize = 1 << 14,
    request_timeout: std::time::Duration = std::time::Duration::from_secs(10),
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

component_utils::gen_unique_id!(pub CallId);

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
#[allow(clippy::type_complexity)]
pub enum Event {
    Response(
        Result<(PeerId, CallId, Vec<u8>, Duration), (Vec<CallId>, StreamUpgradeError<Infallible>)>,
    ),
    Request(PeerId, CallId, Vec<u8>),
}

pub struct Handler {
    requested: bool,
    stream: Option<Result<libp2p::Stream, StreamUpgradeError<Infallible>>>,
    waker: Option<std::task::Waker>,
}

impl ConnectionHandler for Handler {
    type FromBehaviour = Infallible;
    type InboundOpenInfo = ();
    type InboundProtocol = Protocol;
    type OutboundOpenInfo = ();
    type OutboundProtocol = Protocol;
    type ToBehaviour = Result<libp2p::Stream, StreamUpgradeError<Infallible>>;

    fn listen_protocol(
        &self,
    ) -> libp2p::swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        libp2p::swarm::SubstreamProtocol::new(Protocol, ())
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<
        libp2p::swarm::ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
        >,
    > {
        component_utils::set_waker(&mut self.waker, cx.waker());
        if let Some(stream) = self.stream.take() {
            return Poll::Ready(libp2p::swarm::ConnectionHandlerEvent::NotifyBehaviour(
                stream,
            ));
        }

        if !std::mem::replace(&mut self.requested, true) {
            return Poll::Ready(
                libp2p::swarm::ConnectionHandlerEvent::OutboundSubstreamRequest {
                    protocol: SubstreamProtocol::new(Protocol, ()),
                },
            );
        }
        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {}
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
            E::FullyNegotiatedInbound(i) => Ok(i.protocol),
            E::FullyNegotiatedOutbound(o) => Ok(o.protocol),
            E::DialUpgradeError(e) => Err(e.error),
            _ => return,
        };
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
        self.stream = Some(ev);
    }
}

pub struct Protocol;

impl UpgradeInfo for Protocol {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(PROTOCOL_NAME)
    }
}

impl InboundUpgrade<libp2p::Stream> for Protocol {
    type Error = Infallible;
    type Future = std::future::Ready<Result<Self::Output, Self::Error>>;
    type Output = libp2p::Stream;

    fn upgrade_inbound(self, socket: libp2p::Stream, _: Self::Info) -> Self::Future {
        std::future::ready(Ok(socket))
    }
}

impl OutboundUpgrade<libp2p::Stream> for Protocol {
    type Error = Infallible;
    type Future = std::future::Ready<Result<Self::Output, Self::Error>>;
    type Output = libp2p::Stream;

    fn upgrade_outbound(self, socket: libp2p::Stream, _: Self::Info) -> Self::Future {
        std::future::ready(Ok(socket))
    }
}

pub async fn respond(call: CallId, payload: Vec<u8>, mut stream: libp2p::Stream) -> io::Result<()> {
    stream
        .write_all(&(call, Reminder(&payload)).to_packet())
        .await
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
        tracing_subscriber::EnvFilter,
    };

    #[derive(NetworkBehaviour)]
    struct TestBehatiour {
        rpc: Behaviour,
        kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_random_rpc_calls() {
        env_logger::init();

        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();

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
                libp2p::swarm::Config::with_tokio_executor()
                    .with_idle_connection_timeout(Duration::from_secs(10)),
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
            let mut pending_request_count = 0;
            let mut iteration = 0;
            let mut total_requests = 0;
            while total_requests < 1000000 {
                if max_pending_requests > pending_request_count {
                    let peer_id = all_peers[iteration % all_peers.len()];
                    swarm.behaviour_mut().rpc.request(peer_id, [0, 0]);
                    pending_request_count += 1;
                    total_requests += 1;
                }

                let e = libp2p::futures::select! {
                    e = swarm.select_next_some() => e,
                };

                if total_requests % 5000 == 0 {
                    log::info!("total requests: {}", total_requests);
                }

                match e {
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Request(peer, callid, stream),
                    )) => {
                        swarm.behaviour_mut().rpc.respond(peer, callid, stream);
                    }
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Response(Ok(_)),
                    )) => {
                        pending_request_count -= 1;
                    }
                    libp2p::swarm::SwarmEvent::Behaviour(TestBehatiourEvent::Rpc(
                        Event::Response(Err(e)),
                    )) => {
                        log::error!("error: {:?}", e);
                    }
                    e => {
                        log::info!("event: {:?}", e);
                    }
                }

                iteration += 1;
            }
        }

        servers
            .map(|s| run_server(s, peer_ids.clone()))
            .collect::<FuturesUnordered<_>>()
            .next()
            .await;
    }
}
