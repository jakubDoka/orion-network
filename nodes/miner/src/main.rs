#![feature(iter_advance_by)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]

use libp2p::kad::{self, QueryId, QueryResult};

use {
    chain_api::ContractId,
    chat_logic::{DispatchResponse, PacketBuffer, RequestId, SubContext},
    component_utils::{
        codec::Codec,
        kad::KadPeerSearch,
        libp2p::kad::{InboundRequest, StoreInserts},
        Reminder,
    },
    libp2p::{
        core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version},
        futures::{self, StreamExt},
        swarm::{NetworkBehaviour, SwarmEvent},
        Multiaddr, Transport,
    },
    onion::{EncryptedStream, PathId},
    primitives::{chat::*, contracts::NodeData},
    std::{io, mem, net::Ipv4Addr, time::Duration},
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    config::env_config! {
        PORT: u16,
        BOOTSTRAP_NODE: String,
        NODE_ACCOUNT: String,
        NODE_CONTRACT: ContractId,
    }

    let account = if NODE_ACCOUNT.starts_with("//") {
        chain_api::dev_keypair(&NODE_ACCOUNT)
    } else {
        chain_api::mnemonic_keypair(&NODE_ACCOUNT)
    };

    log::info!("booting up node");

    Miner::new(PORT, BOOTSTRAP_NODE, account, NODE_CONTRACT)
        .await
        .run()
        .await;
}

struct Miner {
    swarm: libp2p::swarm::Swarm<Behaviour>,
    peer_discovery: KadPeerSearch,
    clients: futures::stream::SelectAll<Stream>,
    buffer: Vec<u8>,
    bootstrapped: Option<QueryId>,
    server: chat_logic::Server,
    packets: PacketBuffer<RequestId>,
}

impl Miner {
    async fn new(
        port: u16,
        boot_chain_node: String,
        node_account: chain_api::Keypair,
        node_contract: chain_api::ContractId,
    ) -> Self {
        let enc_keys = crypto::enc::KeyPair::new();
        let sig_keys = crypto::sign::KeyPair::new();
        let local_key = libp2p::identity::Keypair::ed25519_from_bytes(sig_keys.ed).unwrap();
        let peer_id = local_key.public().to_peer_id();

        log::info!("peer id: {}", peer_id);
        let client = chain_api::Client::with_signer(&boot_chain_node, node_account)
            .await
            .unwrap();
        log::info!("joined chain");
        client
            .join(
                node_contract,
                NodeData {
                    sign: sig_keys.public_key().into(),
                    enc: enc_keys.public_key().into(),
                }
                .to_stored(),
            )
            .await
            .unwrap();
        log::info!("registered on chain");

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
        .authenticate(libp2p::noise::Config::new(&local_key).unwrap())
        .multiplex(libp2p::yamux::Config::default())
        .or_transport(
            libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default())
                .upgrade(Version::V1)
                .authenticate(libp2p::noise::Config::new(&local_key).unwrap())
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

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4([0; 4].into()))
                    .with(multiaddr::Protocol::Tcp(port)),
            )
            .unwrap();

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4([0; 4].into()))
                    .with(multiaddr::Protocol::Tcp(port + 100))
                    .with(multiaddr::Protocol::Ws("/".into())),
            )
            .unwrap();

        // very fucking important
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        for back_ref in (0..5).filter_map(|i| port.checked_sub(8800 + i)) {
            swarm
                .dial(
                    Multiaddr::empty()
                        .with(multiaddr::Protocol::Ip4(Ipv4Addr::LOCALHOST))
                        .with(multiaddr::Protocol::Tcp(back_ref + 8800)),
                )
                .unwrap();
        }

        Self {
            swarm,
            peer_discovery: Default::default(),
            clients: Default::default(),
            buffer: Default::default(),
            bootstrapped: None,
            server: Default::default(),
            packets: PacketBuffer::new(),
        }
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
        if let Err(e) = self
            .server
            .dispatch(self.swarm.behaviour_mut(), message, &mut self.packets)
        {
            log::error!("failed to dispatch put record message: {}", e);
        }
        self.swarm
            .behaviour_mut()
            .kad
            .store_mut()
            .stop_replication();
        _ = self.packets.drain();
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
                    self.bootstrapped = Some(self.swarm.behaviour_mut().kad.bootstrap().unwrap());
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
            }
            e => log::debug!("{e:?}"),
        }
    }

    fn handle_client_message(&mut self, id: PathId, req: io::Result<Vec<u8>>) {
        let req = match req {
            Ok(req) => req,
            Err(e) => {
                log::error!("failed to read from client: {}", e);
                return;
            }
        };

        let Some(req) = chat_logic::DispatchMessage::decode(&mut req.as_slice()) else {
            log::error!("failed to decode init request");
            return;
        };

        if let Err(e) = self
            .server
            .dispatch(self.swarm.behaviour_mut(), req, &mut self.packets)
        {
            log::error!("failed to dispatch init request: {}", e);
        };

        let stream = self.clients.iter_mut().find(|s| s.assoc == id).unwrap();
        for (rid, packet) in self.packets.drain() {
            let resp = DispatchResponse {
                request_id: rid,
                response: Reminder(packet),
            };
            send_response(resp, &mut stream.inner, &mut self.buffer);
        }

        todo!()
    }

    fn publish_identity(&mut self) {
        todo!()
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
