use std::{collections::VecDeque, convert::Infallible, fmt, io, iter, slice, task::Poll};

use component_utils::{HandlerCore, HandlerRef};
use futures::{AsyncReadExt, AsyncWriteExt, Future};
use libp2p_core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};
use libp2p_identity::PeerId;
use libp2p_swarm::{
    handler::{FullyNegotiatedInbound, FullyNegotiatedOutbound},
    ConnectionHandler, ConnectionHandlerEvent, StreamProtocol,
};
use thiserror::Error;

use crate::{
    packet::{self, CONFIRM_PACKET_SIZE},
    EncryptedStream, KeyPair, PathId, PublicKey, SharedSecret, Stream,
};

const ROUTING_PROTOCOL: StreamProtocol = StreamProtocol::new(concat!(
    "/",
    env!("CARGO_PKG_NAME"),
    "/rot/",
    env!("CARGO_PKG_VERSION"),
));

type Che = ConnectionHandlerEvent<
    <Handler as ConnectionHandler>::OutboundProtocol,
    <Handler as ConnectionHandler>::OutboundOpenInfo,
    <Handler as ConnectionHandler>::ToBehaviour,
    Infallible,
>;

pub struct Handler {
    keypair: Option<KeyPair>,
    buffer_cap: usize,
    clean: bool,
    core: HandlerCore,
    events: VecDeque<Che>,
}

impl Handler {
    pub fn new(keypair: Option<KeyPair>, buffer_cap: usize, should_exist: bool) -> Self {
        log::debug!("new handler, shoudl exist: {:?}", should_exist);
        Self {
            keypair,
            buffer_cap,
            clean: should_exist,
            core: Default::default(),
            events: Default::default(),
        }
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type Error = Infallible;
    type InboundProtocol = IUpgrade;
    type OutboundProtocol = OUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(
        &self,
    ) -> libp2p_swarm::SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        libp2p_swarm::SubstreamProtocol::new(
            IUpgrade {
                keypair: self.keypair.clone(),
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
            log::debug!("no trafic, closing, {:?}", self.clean);
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
            Infallible,
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
                                keypair: self.keypair.clone().unwrap_or_default(),
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
        let ev = match event {
            CE::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: Some(proto),
                ..
            }) => ToBehaviour::IncomingStream(proto),
            CE::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: ChannelMeta { from, to },
                ..
            }) => match from {
                ChannelSource::Stream(from) => ToBehaviour::NewChannel([from, to]),
                ChannelSource::ThisNode(key, id) => ToBehaviour::OutboundStream { to, key, id },
            },
            CE::DialUpgradeError(e) => ToBehaviour::Error(HError::DialUpgrade(e.error)),
            CE::ListenUpgradeError(e) => ToBehaviour::Error(HError::ListenUpgrade(e.error)),
            _ => return,
        };

        self.events.push_back(CHE::NotifyBehaviour(ev));
        self.clean = false;
    }
}

#[derive(Debug, Error)]
pub enum HError {
    #[error("dial upgrade error: {0}")]
    DialUpgrade(libp2p_swarm::StreamUpgradeError<OUpgradeError>),
    #[error("listen upgrade error: {0}")]
    ListenUpgrade(IUpgradeError),
}

#[derive(Debug)]
pub enum ToBehaviour {
    NewChannel([Stream; 2]),
    OutboundStream {
        to: Stream,
        key: SharedSecret,
        id: PathId,
    },
    IncomingStream(IncomingOrResponse),
    Error(HError),
}

#[derive(Debug)]
pub enum FromBehaviour {
    InitPacket(IncomingOrRequest),
}

pub struct IUpgrade {
    keypair: Option<KeyPair>,
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
    type InfoIter = Option<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        self.keypair.as_ref().and(Some(ROUTING_PROTOCOL))
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum IncomingOrRequest {
    Incoming(IncomingStream),
    Request(StreamRequest),
}

#[derive(Debug)]
pub enum IncomingOrResponse {
    Incoming(IncomingStream),
    Response(EncryptedStream),
}

#[derive(Debug)]
pub struct IncomingStream {
    pub(crate) stream: Stream,
    pub(crate) to: PeerId,
    pub(crate) buffer: Vec<u8>,
}

#[derive(Debug)]
pub struct StreamRequest {
    pub(crate) to: PeerId,
    pub(crate) path_id: PathId,
    pub(crate) recipient: PublicKey,
    pub(crate) path: [(PublicKey, PeerId); crate::packet::PATH_LEN],
}

impl InboundUpgrade<libp2p_swarm::Stream> for IUpgrade {
    type Output = Option<IncomingOrResponse>;
    type Error = IUpgradeError;
    type Future = impl Future<Output = Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, mut stream: libp2p_swarm::Stream, proto: Self::Info) -> Self::Future {
        async move {
            let Self {
                keypair,
                buffer_cap,
                rc,
            } = self;

            log::debug!("received inbound stream: {}", proto);
            let mut len = [0; 2];
            stream
                .read_exact(&mut len)
                .await
                .map_err(IUpgradeError::ReadPacketLength)?;

            let len = u16::from_be_bytes(len) as usize;
            let mut buffer = vec![0; len];

            stream
                .read_exact(&mut buffer)
                .await
                .map_err(IUpgradeError::ReadPacket)?;

            log::debug!("peeling packet: {}", len);
            let (to, ss, new_len) =
                crate::packet::peel_initial(&keypair.expect("handshake to fail"), &mut buffer)
                    .ok_or(IUpgradeError::MalformedPacket)?;

            log::debug!("received init packet");
            let Some(to) = to else {
                log::debug!("received incoming stream");
                buffer.resize(CONFIRM_PACKET_SIZE + 1, 0);
                packet::write_confirm(&ss, &mut buffer[1..]);
                buffer[0] = packet::OK;
                stream
                    .write_all(&buffer)
                    .await
                    .map_err(IUpgradeError::WriteAuthPacket)?;

                return Ok(Some(IncomingOrResponse::Response(EncryptedStream::new(
                    Stream::new(stream, buffer_cap, rc),
                    ss,
                ))));
            };

            Ok(Some(IncomingOrResponse::Incoming(IncomingStream {
                stream: Stream::new(stream, buffer_cap, rc),
                to,
                buffer: buffer[..new_len].to_vec(),
            })))
        }
    }
}

#[derive(Debug, Error)]
pub enum IUpgradeError {
    #[error("malformed init packet")]
    MalformedPacket,
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
    keypair: KeyPair,
    incoming: IncomingOrRequest,
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
    ThisNode(SharedSecret, PathId),
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
                keypair,
                incoming,
                buffer_cap,
                rc,
            } = self;

            log::debug!("sending init packet");

            let mut written_packet = vec![];
            let mut ss = [0; 32];
            let buffer = match &incoming {
                IncomingOrRequest::Request(r) => {
                    ss = packet::new_initial(&r.recipient, r.path, &keypair, &mut written_packet)
                        .map_err(OUpgradeError::PacketCreation)?;
                    &written_packet
                }
                IncomingOrRequest::Incoming(i) => &i.buffer,
            };

            stream
                .write_all(&(buffer.len() as u16).to_be_bytes())
                .await
                .map_err(OUpgradeError::WritePacketLength)?;
            stream
                .write_all(buffer)
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
            match kind {
                crate::packet::OK => {
                    let mut buffer = written_packet;
                    buffer.resize(CONFIRM_PACKET_SIZE, 0);
                    stream
                        .read(&mut buffer)
                        .await
                        .map_err(OUpgradeError::ReadPacket)?;

                    if !packet::verify_confirm(&ss, &mut buffer) {
                        Err(OUpgradeError::AuthenticationFailed)
                    } else {
                        Ok(ChannelMeta {
                            from: ChannelSource::ThisNode(ss, request.path_id),
                            to: Stream::new(stream, buffer_cap, rc),
                        })
                    }
                }
                crate::packet::MISSING_PEER => Err(OUpgradeError::MissingPeer),
                crate::packet::OCCUPIED_PEER => Err(OUpgradeError::OccupiedPeer),
                _ => Err(OUpgradeError::UnknownPacketKind(kind)),
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum OUpgradeError {
    #[error("missing peer")]
    MissingPeer,
    #[error("occupied peer")]
    OccupiedPeer,
    #[error("failed to create packet: {0}")]
    PacketCreation(crypto::enc::EncapsulationError),
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
