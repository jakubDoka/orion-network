use std::{array, collections::VecDeque, fmt, io, iter, slice, task::Poll};

use aes_gcm::Aes256Gcm;
use component_utils::{HandlerCore, HandlerRef};
use futures::{AsyncReadExt, AsyncWriteExt, Future};
use libp2p_core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};
use libp2p_identity::PeerId;
use libp2p_swarm::{
    handler::FullyNegotiatedInbound, ConnectionHandler, ConnectionHandlerEvent, StreamProtocol,
};
use thiserror::Error;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::{
    packet::{CONFIRM_PACKET_SIZE, MAX_INIT_PACKET_SIZE},
    PathId, Stream,
};

const ROUTING_PROTOCOL: StreamProtocol = StreamProtocol::new(concat!(
    "/",
    env!("CARGO_PKG_NAME"),
    "/rot/",
    env!("CARGO_PKG_VERSION"),
));

pub(crate) const KEY_SHARE_PROTOCOL: StreamProtocol = StreamProtocol::new(concat!(
    "/",
    env!("CARGO_PKG_NAME"),
    "/shr/",
    env!("CARGO_PKG_VERSION"),
));

pub struct Handler {
    secret: StaticSecret,
    buffer_cap: usize,
    clean: bool,
    core: HandlerCore,
    events: VecDeque<
        ConnectionHandlerEvent<
            <Handler as ConnectionHandler>::OutboundProtocol,
            <Handler as ConnectionHandler>::OutboundOpenInfo,
            <Handler as ConnectionHandler>::ToBehaviour,
            <Handler as ConnectionHandler>::Error,
        >,
    >,
}

impl Handler {
    pub fn new(secret: StaticSecret, buffer_cap: usize, should_exist: bool) -> Self {
        Self {
            secret,
            buffer_cap,
            clean: should_exist,
            core: Default::default(),
            events: Default::default(),
        }
    }
}

#[derive(Debug)]
pub struct OutboundOpenInfo {
    pub from: PeerId,
    pub trough: Option<libp2p_swarm::Stream>,
    pub sender: PublicKey,
    pub to_send: Vec<u8>,
}

impl ConnectionHandler for Handler {
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type Error = HandlerError;
    type InboundProtocol = IUpgrade;
    type OutboundProtocol = OUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(
        &self,
    ) -> libp2p_swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        libp2p_swarm::SubstreamProtocol::new(
            IUpgrade {
                secret: self.secret.clone(),
                buffer_cap: self.buffer_cap,
                rc: self.core.take_ref(),
            },
            (),
        )
    }

    fn connection_keep_alive(&self) -> libp2p_swarm::KeepAlive {
        if self.clean {
            return libp2p_swarm::KeepAlive::Yes;
        }

        if self.core.has_no_trafic() {
            return libp2p_swarm::KeepAlive::No;
        }

        libp2p_swarm::KeepAlive::Yes
    }

    fn poll(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::ToBehaviour,
            Self::Error,
        >,
    > {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            FromBehaviour::InitPacket(incoming) => {
                self.events
                    .push_back(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: libp2p_swarm::SubstreamProtocol::new(
                            OUpgrade {
                                secret: self.secret.clone(),
                                incoming,
                                buffer_cap: self.buffer_cap,
                                rc: self.core.take_ref(),
                            },
                            (),
                        ),
                    });
            }
        }
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
        use libp2p_swarm::handler::ConnectionEvent as CE;
        use libp2p_swarm::handler::ConnectionHandlerEvent as CHE;
        self.events.push_back(match event {
            CE::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: Some(proto),
                ..
            }) => CHE::NotifyBehaviour(ToBehaviour::IncomingStream(proto)),
            CE::FullyNegotiatedOutbound(o) => {
                let ChannelMeta { from, to } = o.protocol;
                match from {
                    ChannelSource::Stream(from) => {
                        CHE::NotifyBehaviour(ToBehaviour::NewChannel([from, to]))
                    }
                    ChannelSource::ThisNode(key, id) => {
                        let key = self.secret.diffie_hellman(&key).to_bytes().into();
                        CHE::NotifyBehaviour(ToBehaviour::OutboundStream { the: to, key, id })
                    }
                }
            }
            CE::DialUpgradeError(e) => CHE::Close(HandlerError::DialUpgrade(e.error)),
            CE::ListenUpgradeError(e) => CHE::Close(HandlerError::ListenUpgrade(e.error)),
            _ => return,
        });

        self.clean = false;
    }
}

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("dial upgrade error: {0}")]
    DialUpgrade(libp2p_swarm::StreamUpgradeError<OUpgradeError>),
    #[error("listen upgrade error: {0}")]
    ListenUpgrade(IUpgradeError),
}

#[derive(Debug)]
pub enum ToBehaviour {
    NewChannel([Stream; 2]),
    OutboundStream {
        the: Stream,
        key: aes_gcm::Key<Aes256Gcm>,
        id: PathId,
    },
    IncomingStream(IncomingOrResponse),
}

#[derive(Debug)]
pub struct IncomingOutput {
    pub stream: libp2p_swarm::Stream,
    pub to: PeerId,
    pub sender: PublicKey,
}

#[derive(Debug)]
pub enum FromBehaviour {
    InitPacket(IncomingOrRequest),
}

pub struct IUpgrade {
    secret: StaticSecret,
    buffer_cap: usize,
    rc: HandlerRef,
}

impl fmt::Debug for IUpgrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IUpgrade")
            .field("secret", &"no you dont")
            .field("buffer_cap", &self.buffer_cap)
            .finish()
    }
}

impl UpgradeInfo for IUpgrade {
    type Info = StreamProtocol;
    type InfoIter = array::IntoIter<Self::Info, 2>;

    fn protocol_info(&self) -> Self::InfoIter {
        [KEY_SHARE_PROTOCOL, ROUTING_PROTOCOL].into_iter()
    }
}

#[derive(Debug)]
pub enum IncomingOrRequest {
    Incoming(IncomingStream),
    Request(StreamRequest),
}

#[derive(Debug)]
pub enum IncomingOrResponse {
    Incoming(IncomingStream),
    Response(StreamResponse),
}

#[derive(Debug)]
pub struct IncomingStream {
    pub stream: Stream,
    pub to: PeerId,
    pub sender: PublicKey,
    pub buffer: Vec<u8>,
}

#[derive(Debug)]
pub struct StreamResponse {
    pub stream: Stream,
    pub sender: PublicKey,
}

#[derive(Debug)]
pub struct StreamRequest {
    pub recipient: PublicKey,
    pub to: PeerId,
    pub sender: PublicKey,
    pub buffer: Vec<u8>,
    pub path_id: PathId,
}

impl InboundUpgrade<libp2p_swarm::Stream> for IUpgrade {
    type Output = Option<IncomingOrResponse>;
    type Error = IUpgradeError;
    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, mut stream: libp2p_swarm::Stream, proto: Self::Info) -> Self::Future {
        async move {
            let Self {
                secret,
                buffer_cap,
                rc,
            } = self;

            log::debug!("received inbound stream: {}", proto);
            if proto != ROUTING_PROTOCOL {
                log::debug!(
                    "received key share stream, sending: {:?}",
                    PublicKey::from(&secret)
                );
                stream
                    .write_all(PublicKey::from(&secret).as_bytes())
                    .await
                    .map_err(IUpgradeError::WriteKeyPacket)?;
                return Ok(None);
            }

            let mut len = [0; 2];
            stream
                .read_exact(&mut len)
                .await
                .map_err(IUpgradeError::ReadPacketLength)?;

            let len = u16::from_be_bytes(len) as usize;
            let mut buffer = vec![0; len];
            if len > MAX_INIT_PACKET_SIZE {
                return Err(IUpgradeError::PacketTooLarge);
            }

            stream
                .read_exact(&mut buffer[..len])
                .await
                .map_err(IUpgradeError::ReadPacket)?;

            let (to, sender, new_len) = crate::packet::peel_initial(&secret, &mut buffer[..len])
                .ok_or(IUpgradeError::MalformedPacket)?;

            log::debug!("received init packet: {:?}", sender);
            let Some(to) = to else {
                log::debug!("received incoming stream");
                buffer.clear();
                crate::packet::wrap(&secret, &sender, &mut buffer);
                assert_eq!(buffer.len(), CONFIRM_PACKET_SIZE);
                buffer.insert(0, crate::packet::OK);
                stream
                    .write_all(&buffer)
                    .await
                    .map_err(IUpgradeError::WriteAuthPacket)?;

                return Ok(Some(IncomingOrResponse::Response(StreamResponse {
                    stream: Stream::new(stream, buffer_cap, rc),
                    sender,
                })));
            };

            Ok(Some(IncomingOrResponse::Incoming(IncomingStream {
                stream: Stream::new(stream, buffer_cap, rc),
                to,
                sender,
                buffer: buffer[..new_len].to_vec(),
            })))
        }
    }
}

#[derive(Debug, Error)]
pub enum IUpgradeError {
    #[error("malformed init packet")]
    MalformedPacket,
    #[error("packet too large")]
    PacketTooLarge,
    #[error("failed to write packet: {0}")]
    WriteKeyPacket(io::Error),
    #[error("failed to read packet length: {0}")]
    ReadPacketLength(io::Error),
    #[error("failed to read packet: {0}")]
    ReadPacket(io::Error),
    #[error("failed to write auth packet: {0}")]
    WriteAuthPacket(io::Error),
}

pub struct OUpgrade {
    incoming: IncomingOrRequest,
    secret: StaticSecret,
    buffer_cap: usize,
    rc: HandlerRef,
}

impl fmt::Debug for OUpgrade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OUpgrade")
            .field("incoming", &self.incoming)
            .field("secret", &"no you dont")
            .field("buffer_cap", &self.buffer_cap)
            .finish()
    }
}

impl UpgradeInfo for OUpgrade {
    type Info = StreamProtocol;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(ROUTING_PROTOCOL)
    }
}

#[derive(Debug)]
pub enum ChannelSource {
    Stream(Stream),
    ThisNode(PublicKey, PathId),
}

#[derive(Debug)]
pub struct ChannelMeta {
    from: ChannelSource,
    to: Stream,
}

impl OutboundUpgrade<libp2p_swarm::Stream> for OUpgrade {
    type Output = ChannelMeta;
    type Error = OUpgradeError;
    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, mut stream: libp2p_swarm::Stream, _: Self::Info) -> Self::Future {
        async move {
            let Self {
                incoming,
                secret,
                buffer_cap,
                rc,
            } = self;

            log::debug!("sending init packet");

            let buffer = match &incoming {
                IncomingOrRequest::Request(r) => &r.buffer,
                IncomingOrRequest::Incoming(i) => &i.buffer,
            };

            stream
                .write_all(&(buffer.len() as u16).to_be_bytes())
                .await
                .map_err(OUpgradeError::WritePacketLength)?;
            stream
                .write_all(&buffer)
                .await
                .map_err(OUpgradeError::WritePacket)?;

            let request = match incoming {
                IncomingOrRequest::Incoming(i) => {
                    log::debug!("received incoming routable stream");
                    return Ok(ChannelMeta {
                        from: ChannelSource::Stream(i.stream),
                        to: Stream::new(stream, buffer_cap, rc),
                    });
                }
                IncomingOrRequest::Request(r) => r,
            };

            let mut kind = 0;
            stream
                .read_exact(slice::from_mut(&mut kind))
                .await
                .map_err(OUpgradeError::ReadPacketKind)?;

            log::debug!("received init packet kind: {}", kind);
            return match kind {
                crate::packet::OK => {
                    let mut buffer = request.buffer;
                    buffer.resize(CONFIRM_PACKET_SIZE, 0);
                    stream
                        .read(&mut buffer)
                        .await
                        .map_err(OUpgradeError::ReadPacket)?;

                    if crate::packet::peel(&secret, &request.recipient, &mut buffer).is_none() {
                        Err(OUpgradeError::AuthenticationFailed)
                    } else {
                        Ok(ChannelMeta {
                            from: ChannelSource::ThisNode(request.recipient, request.path_id),
                            to: Stream::new(stream, buffer_cap, rc),
                        })
                    }
                }
                crate::packet::MISSING_PEER => Err(OUpgradeError::MissingPeer),
                crate::packet::OCCUPIED_PEER => Err(OUpgradeError::OccupiedPeer),
                _ => Err(OUpgradeError::UnknownPacketKind(kind)),
            };
        }
    }
}

#[derive(Debug, Error)]
pub enum OUpgradeError {
    #[error("missing peer")]
    MissingPeer,
    #[error("occupied peer")]
    OccupiedPeer,
    #[error("malformed init packet")]
    MalformedPacket,
    #[error("failed to authenticate")]
    AuthenticationFailed,
    #[error("paket kind not recognized: {0}")]
    UnknownPacketKind(u8),
    #[error("failed to write packet length: {0}")]
    WritePacketLength(io::Error),
    #[error("failed to write packet: {0}")]
    WritePacket(io::Error),
    #[error("failed to read packet kind: {0}")]
    ReadPacketKind(io::Error),
    #[error("failed to read packet: {0}")]
    ReadPacket(io::Error),
}
