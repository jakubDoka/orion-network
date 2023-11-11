use crate::{
    handler::{self, Handler},
    packet::{self, ASOC_DATA, MISSING_PEER},
    IncomingOrRequest, IncomingOrResponse, IncomingStream, KeyPair, PublicKey, SharedSecret,
    StreamRequest,
};
use aes_gcm::{
    aead::{generic_array::GenericArray, OsRng},
    AeadCore, AeadInPlace, Aes256Gcm, KeyInit,
};
use component_utils::PacketReader;
use core::fmt;
use futures::{
    stream::{FusedStream, FuturesUnordered},
    AsyncRead, StreamExt,
};
use instant::{Duration, Instant};
use libp2p::identity::PeerId;
use libp2p::swarm::ToSwarm as TS;
use libp2p::swarm::{
    dial_opts::{DialOpts, PeerCondition},
    CloseConnection, ConnectionId, NetworkBehaviour, NotifyHandler,
};
use std::{
    collections::VecDeque, convert::Infallible, io, mem, ops::DerefMut, pin::Pin, sync::Arc,
    task::Poll,
};
use thiserror::Error;

pub struct Behaviour {
    config: Config,
    router: FuturesUnordered<Channel>,
    peer_to_connection: component_utils::LinearMap<PeerId, ConnectionId>,
    events: VecDeque<TS<Event, handler::FromBehaviour>>,
    pending_connections: Vec<IncomingStream>,
    pending_requests: Vec<StreamRequest>,
    error_streams: FuturesUnordered<component_utils::ClosingStream<libp2p::swarm::Stream>>,
    path_counter: usize,
    buffer: Arc<spin::Mutex<[u8; 1 << 16]>>,
}

impl Behaviour {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            router: Default::default(),
            peer_to_connection: Default::default(),
            events: Default::default(),
            pending_connections: Default::default(),
            pending_requests: Default::default(),
            error_streams: Default::default(),
            path_counter: 0,
            buffer: Arc::new(spin::Mutex::new([0; 1 << 16])),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// # Panics
    ///
    /// Panics if path contains two equal elements in a row.
    pub fn open_path(
        &mut self,
        [mut path @ .., (mut recipient, to)]: [(PublicKey, PeerId); crate::packet::PATH_LEN + 1],
    ) -> Result<PathId, crypto::enc::EncapsulationError> {
        assert!(path.array_windows().all(|[a, b]| a.1 != b.1));

        path.iter_mut()
            .rev()
            .for_each(|(k, _)| mem::swap(k, &mut recipient));

        let path_id = PathId(self.path_counter);
        self.path_counter += 1;

        self.push_stream_request(StreamRequest {
            to,
            recipient,
            path,
            path_id,
        });

        Ok(path_id)
    }

    fn push_stream_request(&mut self, sr: StreamRequest) {
        if sr.to == self.config.current_peer_id {
            // most likely melsrious since we check this in open_path
            log::warn!("melacious stream or self connection");
            return;
        }

        let Some(&conn_id) = self.peer_to_connection.get(&sr.to) else {
            log::debug!(
                "queueing connection to {} from {}",
                sr.to,
                self.config.current_peer_id
            );
            self.events
                .push_back(TS::GenerateEvent(Event::ConnectRequest(sr.to)));
            self.pending_requests.push(sr);
            return;
        };

        self.events.push_back(TS::NotifyHandler {
            peer_id: sr.to,
            handler: NotifyHandler::One(conn_id),
            event: handler::FromBehaviour::InitPacket(IncomingOrRequest::Request(sr)),
        });
    }

    fn push_incoming_stream(&mut self, is: IncomingStream) {
        log::debug!("incoming stream from");
        let valid_stream_count = self
            .router
            .iter_mut()
            .filter_map(|c| c.is_valid(self.config.keep_alive_interval).then_some(()))
            .count();
        if valid_stream_count + self.pending_connections.len() > self.config.max_streams {
            log::info!("too many streams");
            if self.error_streams.len() > self.config.max_error_streams {
                log::warn!("too many erroring streams");
                return;
            }

            let mut stream = is.stream;
            stream.write_error(packet::OCCUPIED_PEER);
            self.error_streams.push(stream.into_closing_stream());
            return;
        }

        if is.to == self.config.current_peer_id {
            // most likely melisious since we check this in open_path
            log::warn!("melacious stream or self connection");
            return;
        }

        let Some(&conn_id) = self.peer_to_connection.get(&is.to) else {
            log::debug!(
                "queueing connection to {} from {}",
                is.to,
                self.config.current_peer_id
            );
            self.events
                .push_back(TS::GenerateEvent(Event::ConnectRequest(is.to)));
            self.pending_connections.push(is);
            return;
        };

        self.events.push_back(TS::NotifyHandler {
            peer_id: is.to,
            handler: NotifyHandler::One(conn_id),
            event: handler::FromBehaviour::InitPacket(IncomingOrRequest::Incoming(is)),
        });
    }

    fn add_connection(&mut self, to: PeerId, to_id: ConnectionId) {
        if self.peer_to_connection.get(&to).is_some() {
            log::debug!("connection to {} already exists", to);
            return;
        }

        self.peer_to_connection.insert(to, to_id);

        let incoming = component_utils::drain_filter(&mut self.pending_connections, |p| p.to != to)
            .map(IncomingOrRequest::Incoming);
        let requests = component_utils::drain_filter(&mut self.pending_requests, |p| p.to != to)
            .map(IncomingOrRequest::Request);

        log::warn!(
            "adding connection to {to} from {}",
            self.config.current_peer_id
        );

        for p in incoming.chain(requests) {
            log::debug!("sending pending stream to {to}");
            self.events.push_back(TS::NotifyHandler {
                peer_id: to,
                handler: NotifyHandler::One(to_id),
                event: handler::FromBehaviour::InitPacket(p),
            });
        }
    }

    /// Must be called when a peer cannot be found, otherwise a pending connection information is
    /// leaked for each `ConnectionRequest`.
    pub fn report_unreachable(&mut self, peer: PeerId) {
        for mut p in component_utils::drain_filter(&mut self.pending_connections, |p| p.to != peer)
        {
            p.stream.write_error(MISSING_PEER);
            self.error_streams.push(p.stream.into_closing_stream());
        }

        for _ in component_utils::drain_filter(&mut self.pending_requests, |p| p.to != peer) {
            self.events
                .push_back(TS::GenerateEvent(Event::Error(Error::MissingPeerLocally)));
        }
    }

    /// Must be called when `ConnectionRequest` is refering to already connected peer but the
    /// behaviour it self is not aware of it.
    pub fn redail(&mut self, peer: PeerId) {
        if self.peer_to_connection.get(&peer).is_some() {
            log::debug!("redail to already connected peer");
            return;
        }

        self.events.push_back(TS::Dial {
            opts: DialOpts::peer_id(peer)
                .condition(PeerCondition::NotDialing)
                .build(),
        });
    }

    fn create_handler(&mut self, peer: PeerId, connection_id: ConnectionId) -> Handler {
        self.add_connection(peer, connection_id);
        Handler::new(self.config.secret.clone(), self.config.buffer_cap)
    }
}

impl component_utils::KadSearchComponent for Behaviour {
    fn redail(&mut self, peer: libp2p::identity::PeerId) {
        self.redail(peer);
    }

    fn mark_failed(&mut self, peer: PeerId) {
        self.report_unreachable(peer);
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;

    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: libp2p::identity::PeerId,
        _local_addr: &libp2p::core::Multiaddr,
        _remote_addr: &libp2p::core::Multiaddr,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(self.create_handler(peer, connection_id))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: libp2p::identity::PeerId,
        _addr: &libp2p::core::Multiaddr,
        _role_override: libp2p::core::Endpoint,
    ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
        Ok(self.create_handler(peer, connection_id))
    }

    fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
        if let libp2p::swarm::FromSwarm::ConnectionClosed(c) = event {
            log::debug!("connection closed to {}", c.peer_id);
            self.peer_to_connection.remove(&c.peer_id);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: libp2p::identity::PeerId,
        connection_id: ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        use crate::handler::ToBehaviour as HTB;
        match event {
            HTB::NewChannel([from, to]) => {
                self.router
                    .push(Channel::new(from, to, self.buffer.clone()))
            }
            HTB::IncomingStream(IncomingOrResponse::Incoming(s)) => self.push_incoming_stream(s),
            HTB::IncomingStream(IncomingOrResponse::Response(s)) => {
                self.events
                    .push_back(TS::GenerateEvent(Event::InboundStream(s)));
            }
            HTB::OutboundStream { to: the, key, id } => {
                self.events
                    .push_back(TS::GenerateEvent(Event::OutboundStream(
                        EncryptedStream::new(the, key),
                        id,
                    )))
            }
            HTB::Error(e) => {
                self.peer_to_connection.remove(&peer_id);
                self.events
                    .push_back(TS::GenerateEvent(Event::Error(Error::Handler(e))));
                self.events.push_back(TS::CloseConnection {
                    peer_id,
                    connection: CloseConnection::One(connection_id),
                })
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<TS<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        if let Poll::Ready(Some(Err(e))) = self.router.poll_next_unpin(cx) {
            self.events
                .push_back(TS::GenerateEvent(Event::Error(Error::RoutedStream(e))));
        }

        if let Poll::Ready(Some(Err(e))) = self.error_streams.poll_next_unpin(cx) {
            self.events
                .push_back(TS::GenerateEvent(Event::Error(Error::ClosingStream(e))));
        }

        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }
}

component_utils::gen_config! {
    /// The secret key used as identiry for the node. In case you pass None, client mode is
    /// enabled and new secret created for each connection.
    secret: Option<KeyPair>,
    /// The peer id of the node.
    current_peer_id: PeerId,
    ;;
    /// The maximum number of streams that can be opened at the same time.
    max_streams: usize = 20,
    /// The maximum number of streams to which the error response is sent.
    max_error_streams: usize = 20,
    /// The maximum interval between two packets before the connection is considered dead.
    keep_alive_interval: std::time::Duration = std::time::Duration::from_secs(10),
    /// size of the buffer for forwarding packets.
    buffer_cap: usize = 1 << 13,
}

#[derive(Debug)]
pub enum Event {
    ConnectRequest(PeerId),
    InboundStream(EncryptedStream),
    OutboundStream(EncryptedStream, PathId),
    Error(Error),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing peer locally")]
    MissingPeerLocally,
    #[error("unexpected error from routed stream: {0}")]
    RoutedStream(io::Error),
    #[error("unexpected error from closing stream: {0}")]
    ClosingStream(io::Error),
    #[error("unexpected error from handler: {0}")]
    Handler(#[from] handler::HError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathId(usize);

#[derive(Debug)]
pub struct EncryptedStream {
    inner: Option<Stream>,
    key: SharedSecret,
    reader: PacketReader,
}

impl EncryptedStream {
    pub(crate) fn new(inner: Stream, key: SharedSecret) -> Self {
        Self {
            inner: Some(inner),
            key,
            reader: Default::default(),
        }
    }

    pub fn write(&mut self, data: &mut [u8]) -> Option<()> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let tag = Aes256Gcm::new(&GenericArray::from(self.key))
            .encrypt_in_place_detached(&nonce, ASOC_DATA, data)
            .unwrap();

        self.inner
            .as_mut()?
            .writer
            .packet(data.iter().cloned().chain(tag).chain(nonce))
            .then_some(())
    }

    pub fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<&mut [u8]>> {
        let Some(stream) = self.inner.as_mut() else {
            return Poll::Pending;
        };

        if let Poll::Ready(Err(e)) = stream.writer.poll(cx, &mut stream.inner) {
            self.inner.take();
            return Poll::Ready(Err(e));
        }

        let read = match futures::ready!(self.reader.poll_packet(cx, &mut stream.inner)) {
            Ok(r) => r,
            Err(err) => {
                self.inner.take();
                return Poll::Ready(Err(err));
            }
        };

        let Some(len) = packet::peel_wih_key(&self.key, read) else {
            self.inner.take();
            return Poll::Ready(Err(io::ErrorKind::InvalidData.into()));
        };

        Poll::Ready(Ok(&mut read[..len]))
    }
}

impl futures::Stream for EncryptedStream {
    type Item = io::Result<Vec<u8>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.is_terminated() {
            return Poll::Ready(None);
        }
        self.poll(cx).map_ok(|v| v.to_vec()).map(Some)
    }
}

impl futures::stream::FusedStream for EncryptedStream {
    fn is_terminated(&self) -> bool {
        self.inner.is_none()
    }
}

#[derive(Debug)]
pub struct Stream {
    inner: libp2p::swarm::Stream,
    writer: component_utils::PacketWriter,
}

impl Stream {
    pub(crate) fn new(inner: libp2p::swarm::Stream, cap: usize) -> Self {
        Self {
            inner,
            writer: component_utils::PacketWriter::new(cap),
        }
    }

    pub fn stream(&mut self) -> &mut libp2p::swarm::Stream {
        &mut self.inner
    }

    pub(crate) fn forward_from(
        &mut self,
        from: &mut libp2p::swarm::Stream,
        temp: &mut [u8],
        last_packet: &mut instant::Instant,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Infallible, io::Error>> {
        loop {
            futures::ready!(self.writer.poll(cx, &mut self.inner))?;
            let n = futures::ready!(Pin::new(&mut *from).poll_read(cx, temp))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
            }
            *last_packet = Instant::now();
            if !self.writer.write(&temp[..n]) {
                return Poll::Ready(Err(io::ErrorKind::InvalidData.into()));
            }
        }
    }

    fn write_error(&mut self, bytes: u8) {
        self.writer.write(&[bytes]);
    }

    fn into_closing_stream(self) -> component_utils::ClosingStream<libp2p::swarm::Stream> {
        component_utils::ClosingStream::new(self.inner, self.writer)
    }
}

pub struct Channel {
    from: Stream,
    to: Stream,
    waker: Option<std::task::Waker>,
    invalid: bool,
    buffer: Arc<spin::Mutex<[u8; 1 << 16]>>,
    last_packet: instant::Instant,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel").finish()
    }
}

impl Channel {
    pub fn new(from: Stream, to: Stream, buffer: Arc<spin::Mutex<[u8; 1 << 16]>>) -> Self {
        Self {
            from,
            to,
            waker: None,
            invalid: false,
            buffer,
            last_packet: instant::Instant::now(),
        }
    }

    pub fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<Infallible>> {
        self.waker = Some(cx.waker().clone());
        let temp = &mut self.buffer.lock()[..];
        if let Poll::Ready(e) =
            self.from
                .forward_from(&mut self.to.inner, temp, &mut self.last_packet, cx)
        {
            return Poll::Ready(e);
        }
        self.to
            .forward_from(&mut self.from.inner, temp, &mut self.last_packet, cx)
    }

    fn is_valid(&mut self, timeout: Duration) -> bool {
        if self.invalid {
            return false;
        }

        if self.last_packet + timeout > instant::Instant::now() {
            return true;
        }

        self.invalid = true;
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        false
    }
}

impl std::future::Future for Channel {
    type Output = Result<Infallible, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        self.deref_mut().poll(cx)
    }
}
