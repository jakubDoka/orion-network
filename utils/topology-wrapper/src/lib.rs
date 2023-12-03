#![feature(impl_trait_in_assoc_type)]
#![feature(macro_metavar_expr)]

pub use impls::{get_extra_events, new, Behaviour};
use libp2p::swarm::{handler::UpgradeInfoSend, ConnectionHandler, NetworkBehaviour};

pub enum ExtraEvent<C: ConnectionHandler> {
    Inbound(<C::InboundProtocol as UpgradeInfoSend>::Info),
    Outbound(<C::OutboundProtocol as UpgradeInfoSend>::Info),
    Disconnected,
}

pub type ExtraEventAndMeta<C> = (
    ExtraEvent<<C as NetworkBehaviour>::ConnectionHandler>,
    libp2p::PeerId,
    libp2p::swarm::ConnectionId,
);

#[cfg(feature = "disabled")]
pub mod report {
    use {crate::ExtraEventAndMeta, libp2p::swarm::NetworkBehaviour};
    pub type Behaviour = libp2p::swarm::dummy::Behaviour;
    pub fn report<T: NetworkBehaviour>(
        _: &mut Behaviour,
        _: impl Iterator<Item = ExtraEventAndMeta<T>>,
    ) {
    }
}
#[cfg(feature = "disabled")]
mod impls {
    use {crate::ExtraEventAndMeta, libp2p::swarm::NetworkBehaviour};

    pub type Behaviour<T> = T;
    pub fn new<T>(inner: T) -> T {
        inner
    }
    pub fn get_extra_events<T: NetworkBehaviour>(
        _: &mut Behaviour<T>,
    ) -> impl Iterator<Item = ExtraEventAndMeta<T>> {
        std::iter::empty()
    }
}

#[cfg(not(feature = "disabled"))]
pub mod collector {
    use {
        crate::report::Update,
        component_utils::Codec,
        libp2p::{
            futures::{stream::SelectAll, StreamExt},
            swarm::{dial_opts::DialOpts, FromSwarm, NetworkBehaviour},
            PeerId,
        },
        std::{convert::Infallible, io},
    };

    pub trait World: 'static {
        fn handle_update(&mut self, peer: PeerId, update: Update);
        fn disconnect(&mut self, peer: PeerId);
    }

    struct UpdateStream {
        peer: libp2p::PeerId,
        inner: libp2p::Stream,
        reader: component_utils::stream::PacketReader,
    }

    impl libp2p::futures::Stream for UpdateStream {
        type Item = (io::Result<Vec<u8>>, PeerId);

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let s = self.get_mut();
            s.reader
                .poll_packet(cx, &mut s.inner)
                .map(|r| Some((r.map(|r| r.to_vec()), s.peer)))
        }
    }

    pub struct Behaviour<W: World> {
        world: W,
        listeners: SelectAll<UpdateStream>,
        pending_connections: Vec<PeerId>,
    }

    impl<W: World> Behaviour<W> {
        pub fn new(world: W) -> Self {
            Self {
                world,
                listeners: Default::default(),
                pending_connections: Default::default(),
            }
        }

        pub fn world_mut(&mut self) -> &mut W {
            &mut self.world
        }

        pub fn add_peer(&mut self, addr: PeerId) {
            self.pending_connections.push(addr);
        }
    }

    impl<W: World> NetworkBehaviour for Behaviour<W> {
        type ConnectionHandler = crate::report::Handler;
        type ToSwarm = Infallible;

        fn handle_established_inbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _peer: PeerId,
            _local_addr: &libp2p::Multiaddr,
            _remote_addr: &libp2p::Multiaddr,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            Ok(crate::report::Handler::connecting())
        }

        fn handle_established_outbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _peer: PeerId,
            _addr: &libp2p::Multiaddr,
            _role_override: libp2p::core::Endpoint,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            Ok(crate::report::Handler::connecting())
        }

        fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
            if let FromSwarm::ConnectionClosed(c) = event {
                self.world.disconnect(c.peer_id);
            }
        }

        fn on_connection_handler_event(
            &mut self,
            peer_id: PeerId,
            _connection_id: libp2p::swarm::ConnectionId,
            event: libp2p::swarm::THandlerOutEvent<Self>,
        ) {
            if self.listeners.iter().any(|l| l.peer == peer_id) {
                return;
            }
            self.listeners.push(UpdateStream {
                peer: peer_id,
                inner: event,
                reader: component_utils::stream::PacketReader::default(),
            });
        }

        fn poll(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>,
        > {
            if let Some(peer) = self.pending_connections.pop() {
                return std::task::Poll::Ready(libp2p::swarm::ToSwarm::Dial {
                    opts: DialOpts::peer_id(peer).build(),
                });
            }

            let Some((packet, peer)) = libp2p::futures::ready!(self.listeners.poll_next_unpin(cx))
            else {
                return std::task::Poll::Pending;
            };

            let Ok(packet) =
                packet.inspect_err(|e| log::info!("Error while reading update: {}", e))
            else {
                return std::task::Poll::Pending;
            };

            let Some(update) = Vec::<Update>::decode(&mut packet.as_slice()) else {
                log::info!("Invalid update received, {:?}", packet);
                return std::task::Poll::Pending;
            };

            for update in update {
                self.world.handle_update(peer, update);
            }

            std::task::Poll::Pending
        }
    }
}

#[cfg(not(feature = "disabled"))]
pub mod report {
    use {
        crate::{ExtraEvent, ExtraEventAndMeta},
        component_utils::{Codec, PacketWriter},
        libp2p::{
            core::UpgradeInfo,
            futures::{stream::FuturesUnordered, StreamExt},
            swarm::{ConnectionHandler, NetworkBehaviour},
            PeerId, StreamProtocol,
        },
        std::{convert::Infallible, io, ops::DerefMut},
    };

    pub fn new() -> Behaviour {
        Behaviour {
            listeners: Default::default(),
        }
    }

    pub fn report<T: NetworkBehaviour>(
        r: &mut Behaviour,
        events: impl Iterator<Item = ExtraEventAndMeta<T>>,
    ) {
        let events = events.collect::<Vec<_>>();
        if events.is_empty() {
            return;
        }
        let events = events
            .iter()
            .map(|(e, p, c)| {
                let e = match e {
                    ExtraEvent::Inbound(i) => Event::Inbound(i.as_ref()),
                    ExtraEvent::Outbound(o) => Event::Outbound(o.as_ref()),
                    ExtraEvent::Disconnected => Event::Disconnected,
                };

                Update {
                    event: e,
                    peer: *p,
                    connection: unsafe { std::mem::transmute(*c) },
                }
            })
            .collect::<Vec<_>>()
            .to_bytes();
        for l in r.listeners.iter_mut() {
            l.writer.packet(events.iter().copied());
        }
    }

    component_utils::protocol! {'a:
        struct Update<'a> {
            event: Event<'a>,
            peer: PeerId,
            connection: usize,
        }

        enum Event<'a> {
            Inbound: &'a str,
            Outbound: &'a str,
            Disconnected,
        }
    }

    struct UpdateStream {
        peer: PeerId,
        inner: libp2p::Stream,
        writer: component_utils::stream::PacketWriter,
    }

    impl std::future::Future for UpdateStream {
        type Output = io::Result<()>;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let s = self.deref_mut();
            libp2p::futures::ready!(s.writer.poll(cx, &mut s.inner))?;
            std::task::Poll::Pending
        }
    }

    pub struct Behaviour {
        listeners: FuturesUnordered<UpdateStream>,
    }

    impl NetworkBehaviour for Behaviour {
        type ConnectionHandler = Handler;
        type ToSwarm = Infallible;

        fn handle_established_inbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _peer: PeerId,
            _local_addr: &libp2p::Multiaddr,
            _remote_addr: &libp2p::Multiaddr,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            Ok(Handler::default())
        }

        fn handle_established_outbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _peer: PeerId,
            _addr: &libp2p::Multiaddr,
            _role_override: libp2p::core::Endpoint,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            Ok(Handler::default())
        }

        fn on_swarm_event(&mut self, _event: libp2p::swarm::FromSwarm) {}

        fn on_connection_handler_event(
            &mut self,
            peer_id: PeerId,
            _connection_id: libp2p::swarm::ConnectionId,
            event: libp2p::swarm::THandlerOutEvent<Self>,
        ) {
            if self.listeners.iter().any(|l| l.peer == peer_id) {
                return;
            }
            self.listeners.push(UpdateStream {
                peer: peer_id,
                inner: event,
                writer: PacketWriter::new(1 << 14),
            });
        }

        fn poll(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>,
        > {
            if let std::task::Poll::Ready(Some(Err(e))) = self.listeners.poll_next_unpin(cx) {
                log::info!("Error while writing update: {}", e);
            }
            std::task::Poll::Pending
        }
    }

    #[derive(Default)]
    pub struct Handler {
        connected: Option<libp2p::Stream>,
        connect: bool,
    }

    impl Handler {
        pub fn connecting() -> Self {
            Self {
                connected: None,
                connect: true,
            }
        }
    }

    impl ConnectionHandler for Handler {
        type FromBehaviour = Infallible;
        type InboundOpenInfo = ();
        type InboundProtocol = Protocol;
        type OutboundOpenInfo = ();
        type OutboundProtocol = Protocol;
        type ToBehaviour = libp2p::Stream;

        fn listen_protocol(
            &self,
        ) -> libp2p::swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo>
        {
            libp2p::swarm::SubstreamProtocol::new(Protocol, ())
        }

        fn poll(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            libp2p::swarm::ConnectionHandlerEvent<
                Self::OutboundProtocol,
                Self::OutboundOpenInfo,
                Self::ToBehaviour,
            >,
        > {
            if self.connect {
                self.connect = false;
                return std::task::Poll::Ready(
                    libp2p::swarm::ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: libp2p::swarm::SubstreamProtocol::new(Protocol, ()),
                    },
                );
            }

            if let Some(stream) = self.connected.take() {
                return std::task::Poll::Ready(
                    libp2p::swarm::ConnectionHandlerEvent::NotifyBehaviour(stream),
                );
            }

            std::task::Poll::Pending
        }

        fn on_behaviour_event(&mut self, _event: Self::FromBehaviour) {}

        fn on_connection_event(
            &mut self,
            event: libp2p::swarm::handler::ConnectionEvent<
                Self::InboundProtocol,
                Self::OutboundProtocol,
                Self::InboundOpenInfo,
                Self::OutboundOpenInfo,
            >,
        ) {
            match event {
                libp2p::swarm::handler::ConnectionEvent::FullyNegotiatedInbound(i) => {
                    self.connected = Some(i.protocol);
                }
                libp2p::swarm::handler::ConnectionEvent::FullyNegotiatedOutbound(o) => {
                    self.connected = Some(o.protocol);
                }
                _ => {}
            }
        }
    }

    const ROUTING_PROTOCOL: StreamProtocol = StreamProtocol::new(concat!(
        "/",
        env!("CARGO_PKG_NAME"),
        "/updt/",
        env!("CARGO_PKG_VERSION"),
    ));

    pub struct Protocol;

    impl UpgradeInfo for Protocol {
        type Info = StreamProtocol;
        type InfoIter = std::iter::Once<Self::Info>;

        fn protocol_info(&self) -> Self::InfoIter {
            std::iter::once(ROUTING_PROTOCOL)
        }
    }

    impl libp2p::core::upgrade::InboundUpgrade<libp2p::Stream> for Protocol {
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Output, Self::Error>>;
        type Output = libp2p::Stream;

        fn upgrade_inbound(self, socket: libp2p::Stream, _: Self::Info) -> Self::Future {
            std::future::ready(Ok(socket))
        }
    }

    impl libp2p::core::upgrade::OutboundUpgrade<libp2p::Stream> for Protocol {
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Output, Self::Error>>;
        type Output = libp2p::Stream;

        fn upgrade_outbound(self, socket: libp2p::Stream, _: Self::Info) -> Self::Future {
            std::future::ready(Ok(socket))
        }
    }
}

#[cfg(not(feature = "disabled"))]
mod impls {
    use {
        crate::{ExtraEvent, ExtraEventAndMeta},
        libp2p::{
            futures::TryFutureExt,
            swarm::{
                handler::{
                    DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
                    InboundUpgradeSend, ListenUpgradeError, OutboundUpgradeSend, UpgradeInfoSend,
                },
                ConnectionHandler, NetworkBehaviour,
            },
        },
        std::{
            collections::VecDeque,
            ops::{Deref, DerefMut},
        },
    };

    pub fn new<T: NetworkBehaviour>(inner: T) -> Behaviour<T> {
        Behaviour::new(inner)
    }

    pub fn get_extra_events<T: NetworkBehaviour>(
        behaviour: &mut Behaviour<T>,
    ) -> impl Iterator<Item = ExtraEventAndMeta<T>> + '_ {
        behaviour.get_extra_events()
    }

    pub struct Behaviour<T: NetworkBehaviour> {
        inner: T,
        extra_events: VecDeque<ExtraEventAndMeta<T>>,
    }

    impl<T: NetworkBehaviour> Behaviour<T> {
        fn new(inner: T) -> Self {
            Self {
                inner,
                extra_events: VecDeque::new(),
            }
        }

        fn get_extra_events(&mut self) -> impl Iterator<Item = ExtraEventAndMeta<T>> + '_ {
            self.extra_events.drain(..)
        }
    }

    impl<T: NetworkBehaviour> NetworkBehaviour for Behaviour<T> {
        type ConnectionHandler = Handler<T::ConnectionHandler>;
        type ToSwarm = T::ToSwarm;

        fn handle_established_inbound_connection(
            &mut self,
            connection_id: libp2p::swarm::ConnectionId,
            peer: libp2p::PeerId,
            local_addr: &libp2p::Multiaddr,
            remote_addr: &libp2p::Multiaddr,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            self.inner
                .handle_established_inbound_connection(connection_id, peer, local_addr, remote_addr)
                .map(Handler::new)
        }

        fn handle_established_outbound_connection(
            &mut self,
            connection_id: libp2p::swarm::ConnectionId,
            peer: libp2p::PeerId,
            addr: &libp2p::Multiaddr,
            role_override: libp2p::core::Endpoint,
        ) -> Result<libp2p::swarm::THandler<Self>, libp2p::swarm::ConnectionDenied> {
            self.inner
                .handle_established_outbound_connection(connection_id, peer, addr, role_override)
                .map(Handler::new)
        }

        fn on_swarm_event(&mut self, event: libp2p::swarm::FromSwarm) {
            self.inner.on_swarm_event(event)
        }

        fn on_connection_handler_event(
            &mut self,
            peer_id: libp2p::PeerId,
            connection_id: libp2p::swarm::ConnectionId,
            event: libp2p::swarm::THandlerOutEvent<Self>,
        ) {
            let event = match event {
                ToBehavior::Inner(i) => i,
                ToBehavior::Extra(e) => {
                    self.extra_events.push_back((e, peer_id, connection_id));
                    return;
                }
            };
            self.inner
                .on_connection_handler_event(peer_id, connection_id, event)
        }

        fn poll(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>,
        > {
            self.inner.poll(cx)
        }

        fn handle_pending_inbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _local_addr: &libp2p::Multiaddr,
            _remote_addr: &libp2p::Multiaddr,
        ) -> Result<(), libp2p::swarm::ConnectionDenied> {
            self.inner
                .handle_pending_inbound_connection(_connection_id, _local_addr, _remote_addr)
        }

        fn handle_pending_outbound_connection(
            &mut self,
            _connection_id: libp2p::swarm::ConnectionId,
            _maybe_peer: Option<libp2p::PeerId>,
            _addresses: &[libp2p::Multiaddr],
            _effective_role: libp2p::core::Endpoint,
        ) -> Result<Vec<libp2p::Multiaddr>, libp2p::swarm::ConnectionDenied> {
            self.inner.handle_pending_outbound_connection(
                _connection_id,
                _maybe_peer,
                _addresses,
                _effective_role,
            )
        }
    }

    impl<T: NetworkBehaviour> Deref for Behaviour<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<T: NetworkBehaviour> DerefMut for Behaviour<T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    pub struct Handler<T: ConnectionHandler> {
        inner: T,
        extra_events: VecDeque<ExtraEvent<T>>,
    }

    impl<T: ConnectionHandler> Handler<T> {
        pub fn new(inner: T) -> Self {
            Self {
                inner,
                extra_events: VecDeque::new(),
            }
        }
    }

    impl<T: ConnectionHandler> ConnectionHandler for Handler<T> {
        type FromBehaviour = T::FromBehaviour;
        type InboundOpenInfo = T::InboundOpenInfo;
        type InboundProtocol = Protocol<T::InboundProtocol>;
        type OutboundOpenInfo = T::OutboundOpenInfo;
        type OutboundProtocol = Protocol<T::OutboundProtocol>;
        type ToBehaviour = ToBehavior<T>;

        fn listen_protocol(
            &self,
        ) -> libp2p::swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo>
        {
            self.inner.listen_protocol().map_upgrade(Protocol::new)
        }

        fn poll(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<
            libp2p::swarm::ConnectionHandlerEvent<
                Self::OutboundProtocol,
                Self::OutboundOpenInfo,
                Self::ToBehaviour,
            >,
        > {
            if let Some(event) = self.extra_events.pop_front() {
                return std::task::Poll::Ready(
                    libp2p::swarm::ConnectionHandlerEvent::NotifyBehaviour(ToBehavior::Extra(
                        event,
                    )),
                );
            }

            self.inner
                .poll(cx)
                .map(|e| e.map_custom(ToBehavior::Inner).map_protocol(Protocol::new))
        }

        fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
            self.inner.on_behaviour_event(event)
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
            use libp2p::swarm::handler::ConnectionEvent as CE;
            let event = match event {
                CE::FullyNegotiatedInbound(i) => {
                    self.extra_events
                        .push_back(ExtraEvent::Inbound(i.protocol.1));
                    libp2p::swarm::handler::ConnectionEvent::FullyNegotiatedInbound(
                        FullyNegotiatedInbound {
                            protocol: i.protocol.0,
                            info: i.info,
                        },
                    )
                }
                CE::FullyNegotiatedOutbound(o) => {
                    self.extra_events
                        .push_back(ExtraEvent::Outbound(o.protocol.1));
                    CE::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                        protocol: o.protocol.0,
                        info: o.info,
                    })
                }
                CE::AddressChange(a) => CE::AddressChange(a),
                CE::DialUpgradeError(d) => CE::DialUpgradeError(DialUpgradeError {
                    error: d.error,
                    info: d.info,
                }),
                CE::ListenUpgradeError(l) => CE::ListenUpgradeError(ListenUpgradeError {
                    error: l.error,
                    info: l.info,
                }),
                CE::LocalProtocolsChange(l) => CE::LocalProtocolsChange(l),
                CE::RemoteProtocolsChange(r) => CE::RemoteProtocolsChange(r),
                _ => return,
            };
            self.inner.on_connection_event(event)
        }

        fn connection_keep_alive(&self) -> bool {
            self.inner.connection_keep_alive()
        }

        fn poll_close(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::ToBehaviour>> {
            self.inner
                .poll_close(cx)
                .map(|opt| opt.map(ToBehavior::Inner))
        }
    }

    pub enum ToBehavior<C: ConnectionHandler> {
        Inner(C::ToBehaviour),
        Extra(ExtraEvent<C>),
    }

    impl<C: ConnectionHandler> std::fmt::Debug for ToBehavior<C> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ToBehavior::Inner(_) => write!(f, "ToBehavior::Inner"),
                ToBehavior::Extra(_) => write!(f, "ToBehavior::Extra"),
            }
        }
    }

    pub struct Protocol<T> {
        inner: T,
    }

    impl<T> Protocol<T> {
        pub fn new(inner: T) -> Self {
            Self { inner }
        }
    }

    impl<T: UpgradeInfoSend> UpgradeInfoSend for Protocol<T> {
        type Info = T::Info;
        type InfoIter = T::InfoIter;

        fn protocol_info(&self) -> Self::InfoIter {
            self.inner.protocol_info()
        }
    }

    impl<T: InboundUpgradeSend> InboundUpgradeSend for Protocol<T> {
        type Error = T::Error;
        type Output = (T::Output, Self::Info);

        type Future = impl std::future::Future<Output = Result<Self::Output, Self::Error>>;

        fn upgrade_inbound(self, socket: libp2p::Stream, info: Self::Info) -> Self::Future {
            self.inner
                .upgrade_inbound(socket, info.clone())
                .map_ok(|o| (o, info))
        }
    }

    impl<T: OutboundUpgradeSend> OutboundUpgradeSend for Protocol<T> {
        type Error = T::Error;
        type Output = (T::Output, Self::Info);

        type Future = impl std::future::Future<Output = Result<Self::Output, Self::Error>>;

        fn upgrade_outbound(self, socket: libp2p::Stream, info: Self::Info) -> Self::Future {
            self.inner
                .upgrade_outbound(socket, info.clone())
                .map_ok(|o| (o, info))
        }
    }
}
