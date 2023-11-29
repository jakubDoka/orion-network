#![feature(iter_advance_by)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]

use {
    anyhow::Context,
    chain_api::ContractId,
    chat_logic::{DispatchResponse, PublishNode, RootPacketBuffer, SubContext, REPLICATION_FACTOR},
    component_utils::{
        codec::Codec,
        kad::KadPeerSearch,
        libp2p::kad::{InboundRequest, StoreInserts},
        Reminder,
    },
    crypto::{enc, sign, TransmutationCircle},
    libp2p::{
        core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version},
        futures::{self, StreamExt},
        kad::{self, QueryId, QueryResult},
        swarm::{NetworkBehaviour, SwarmEvent},
        Multiaddr, Transport,
    },
    onion::{EncryptedStream, PathId},
    primitives::contracts::{NodeData, NodeIdentity},
    std::{fs, io, mem, thread, time::Duration},
};

#[derive(Default, Clone)]
struct NodeKeys {
    enc: enc::KeyPair,
    sign: sign::KeyPair,
}

crypto::impl_transmute! {
    NodeKeys,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    Miner::new().await?.run().await;

    Ok(())
}

struct Miner {
    swarm: libp2p::swarm::Swarm<Behaviour>,
    peer_discovery: KadPeerSearch,
    clients: futures::stream::SelectAll<Stream>,
    buffer: Vec<u8>,
    bootstrapped: Option<QueryId>,
    server: chat_logic::Server,
    packets: RootPacketBuffer,
    sign: crypto::sign::KeyPair,
}

impl Miner {
    async fn new() -> anyhow::Result<Self> {
        config::env_config! {
            PORT: u16,
            WS_PORT: u16,
            CHAIN_NODE: String,
            NODE_ACCOUNT: String,
            NODE_CONTRACT: ContractId,
            KEY_PATH: String,
            BOOT_NODES: config::List<Multiaddr>,
        }

        let account = if NODE_ACCOUNT.starts_with("//") {
            chain_api::dev_keypair(&NODE_ACCOUNT)
        } else {
            chain_api::mnemonic_keypair(&NODE_ACCOUNT)
        };

        let (enc_keys, sign_keys, is_new) =
            Self::load_keys(KEY_PATH).context("key file loading")?;
        let local_key = libp2p::identity::Keypair::ed25519_from_bytes(sign_keys.ed)
            .context("deriving ed signature")?;
        let peer_id = local_key.public().to_peer_id();

        log::info!("peer id: {}", peer_id);

        if is_new {
            let client = chain_api::Client::with_signer(&CHAIN_NODE, account)
                .await
                .context("connecting to chain")?;
            client
                .join(
                    NODE_CONTRACT,
                    NodeData {
                        sign: sign_keys.public_key(),
                        enc: enc_keys.public_key(),
                    }
                    .to_stored(),
                )
                .await
                .context("registeing to chain")?;
            log::info!("registered on chain");
        }

        let behaviour = Behaviour {
            onion: onion::Behaviour::new(
                onion::Config::new(enc_keys.clone().into(), peer_id)
                    .max_streams(10)
                    .keep_alive_interval(Duration::from_secs(100)),
            ),
            kad: kad::Behaviour::with_config(
                peer_id,
                chat_logic::Storage::new(),
                mem::take(
                    kad::Config::default()
                        .set_replication_factor(REPLICATION_FACTOR)
                        .set_record_filtering(StoreInserts::FilterBoth),
                ),
            ),
            identfy: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "0.1.0".into(),
                local_key.public(),
            )),
        };
        let transport = libp2p::websocket::WsConfig::new(libp2p::tcp::tokio::Transport::new(
            libp2p::tcp::Config::default(),
        ))
        .upgrade(Version::V1)
        .authenticate(libp2p::noise::Config::new(&local_key).context("noise initialization")?)
        .multiplex(libp2p::yamux::Config::default())
        .or_transport(
            libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default())
                .upgrade(Version::V1)
                .authenticate(
                    libp2p::noise::Config::new(&local_key).context("noise initialization")?,
                )
                .multiplex(libp2p::yamux::Config::default()),
        )
        .map(|t, _| match t {
            futures::future::Either::Left((p, m)) => (p, StreamMuxerBox::new(m)),
            futures::future::Either::Right((p, m)) => (p, StreamMuxerBox::new(m)),
        })
        .boxed();
        let mut swarm = libp2p::swarm::Swarm::new(
            transport,
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(Duration::MAX),
        );

        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4([0; 4].into()))
                    .with(multiaddr::Protocol::Tcp(PORT)),
            )
            .context("starting to listen for peers")?;

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4([0; 4].into()))
                    .with(multiaddr::Protocol::Tcp(WS_PORT))
                    .with(multiaddr::Protocol::Ws("/".into())),
            )
            .context("starting to isten for clients")?;

        tokio::time::sleep(Duration::from_secs(1)).await;

        for boot_node in BOOT_NODES.0 {
            swarm.dial(boot_node).context("dialing a boot peer")?;
        }

        Ok(Self {
            swarm,
            peer_discovery: Default::default(),
            clients: Default::default(),
            buffer: Default::default(),
            bootstrapped: None,
            server: Default::default(),
            packets: RootPacketBuffer::new(),
            sign: sign_keys,
        })
    }

    fn load_keys(path: String) -> io::Result<(enc::KeyPair, sign::KeyPair, bool)> {
        let file = match fs::read(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let nk = NodeKeys::default();
                fs::write(&path, nk.as_bytes())?;
                return Ok((nk.enc, nk.sign, true));
            }
            Err(e) => return Err(e),
        };

        let Some(nk) = NodeKeys::try_from_slice(&file).cloned() else {
            return Err(io::Error::other("invalid key file"));
        };

        Ok((nk.enc, nk.sign, false))
    }

    fn handle_put_record(&mut self, record: kad::Record) {
        let Some(message) = chat_logic::DispatchMessage::decode(&mut record.value.as_slice())
        else {
            log::warn!("invalid put record message");
            return;
        };

        self.swarm
            .behaviour_mut()
            .kad
            .store_mut()
            .start_replication();
        if let Err(e) =
            self.server
                .dispatch(self.swarm.behaviour_mut(), message, None, &mut self.packets)
        {
            log::error!("failed to dispatch put record message: {}", e);
        }
        self.swarm
            .behaviour_mut()
            .kad
            .store_mut()
            .stop_replication();
        _ = self.packets.drain();

        self.dispatch_events();
        log::info!("put record message");
    }

    fn handle_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Identfy(libp2p::identify::Event::Received {
                peer_id,
                info,
            })) => {
                for addr in info.listen_addrs {
                    self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                }

                if self.bootstrapped.is_none() {
                    self.bootstrapped = Some(
                        self.swarm
                            .behaviour_mut()
                            .kad
                            .bootstrap()
                            .expect("we now have at least one node connected"),
                    );
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest(to))) => {
                component_utils::handle_conn_request(to, &mut self.swarm, &mut self.peer_discovery)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result: QueryResult::Bootstrap(Ok(_)),
                step,
                ..
            })) if Some(id) == self.bootstrapped && step.last => {
                log::info!("bootstrapped");
                self.publish_identity();
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(
                    &e,
                    &mut self.swarm,
                    &mut self.peer_discovery,
                ) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::InboundStream(
                inner,
                id,
            ))) => {
                self.clients.push(Stream { assoc: id, inner });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::InboundRequest {
                request:
                    InboundRequest::PutRecord {
                        record: Some(record),
                        ..
                    },
            })) => self.handle_put_record(record),
            SwarmEvent::Behaviour(b) => {
                let _ =
                    self.server
                        .try_handle_event(self.swarm.behaviour_mut(), b, &mut self.packets);

                for ((rid, id), packet) in self.packets.drain() {
                    let resp = DispatchResponse {
                        request_id: rid,
                        response: Reminder(packet),
                    };
                    let Some(stream) = self.clients.iter_mut().find(|s| s.assoc == id) else {
                        continue;
                    };
                    send_response(resp, &mut stream.inner, &mut self.buffer);
                }

                self.dispatch_events();
            }
            e => log::debug!("{e:?}"),
        }
    }

    fn handle_client_message(&mut self, id: PathId, req: io::Result<Vec<u8>>) {
        let req = match req {
            Ok(req) => req,
            Err(e) => {
                self.server.disconnected(id);
                log::error!("failed to read from client: {}", e);
                return;
            }
        };

        let Some(req) = chat_logic::DispatchMessage::decode(&mut req.as_slice()) else {
            log::error!("failed to decode init request: {:?}", req);
            return;
        };

        if let Err(e) =
            self.server
                .dispatch(self.swarm.behaviour_mut(), req, Some(id), &mut self.packets)
        {
            log::error!("failed to dispatch init request: {}", e);
            return;
        };

        let stream = self
            .clients
            .iter_mut()
            .find(|s| s.assoc == id)
            .expect("we just received message");
        for ((rid, _), packet) in self.packets.drain() {
            let resp = DispatchResponse {
                request_id: rid,
                response: Reminder(packet),
            };
            send_response(resp, &mut stream.inner, &mut self.buffer);
        }

        self.dispatch_events();
    }

    fn dispatch_events(&mut self) {
        for (targets, event) in self
            .server
            .smsg
            .drain_events()
            .chain(self.server.sm.drain_events())
        {
            for (target, request_id) in targets {
                let resp = DispatchResponse {
                    request_id,
                    response: Reminder(event),
                };
                let Some(stream) = self.clients.iter_mut().find(|s| s.assoc == target) else {
                    continue;
                };
                send_response(resp, &mut stream.inner, &mut self.buffer);
            }
        }
    }

    fn publish_identity(&mut self) {
        let signature = NodeIdentity {
            sign: self.sign.public_key(),
            enc: self
                .swarm
                .behaviour()
                .onion
                .config()
                .secret
                .clone()
                .expect("we are the server")
                .public_key(),
        };
        self.server.dispatch_local::<PublishNode>(
            &mut self.swarm.behaviour_mut().kad,
            signature.into_bytes(),
        )
    }

    async fn run(mut self) {
        loop {
            futures::select! {
                e = self.swarm.select_next_some() => self.handle_event(e),
                (id, m) = self.clients.select_next_some() => self.handle_client_message(id, m),
            };
        }
    }
}

pub fn send_response<'a, T: Codec<'a>>(
    resp: T,
    stream: &mut EncryptedStream,
    buffer: &mut Vec<u8>,
) {
    buffer.clear();
    resp.encode(buffer);
    stream.write(buffer);
}

type SE = libp2p::swarm::SwarmEvent<<Behaviour as NetworkBehaviour>::ToSwarm>;

#[derive(NetworkBehaviour)]
struct Behaviour {
    onion: onion::Behaviour,
    kad: kad::Behaviour<chat_logic::Storage>,
    identfy: libp2p::identify::Behaviour,
}

impl From<libp2p::kad::Event> for BehaviourEvent {
    fn from(e: libp2p::kad::Event) -> Self {
        BehaviourEvent::Kad(e)
    }
}

impl SubContext<libp2p::kad::Behaviour<chat_logic::Storage>> for Behaviour {
    fn fragment(&mut self) -> &mut libp2p::kad::Behaviour<chat_logic::Storage> {
        &mut self.kad
    }

    fn try_unpack_event(
        event: Self::ToSwarm,
    ) -> Result<
        <libp2p::kad::Behaviour<chat_logic::Storage> as chat_logic::Context>::ToSwarm,
        Self::ToSwarm,
    > {
        match event {
            BehaviourEvent::Kad(e) => Ok(e),
            _ => Err(event),
        }
    }
}

component_utils::impl_kad_search!(Behaviour => (chat_logic::Storage, onion::Behaviour => onion, kad));

type Stream = component_utils::stream::AsocStream<PathId, EncryptedStream>;
