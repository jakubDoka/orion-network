#![feature(iter_advance_by)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]
#![feature(macro_metavar_expr)]

use {
    self::handlers::RequestOrigin,
    anyhow::Context,
    chain_api::ContractId,
    chat_logic::*,
    component_utils::{kad::KadPeerSearch, libp2p::kad::StoreInserts, Reminder},
    crypto::{enc, sign, TransmutationCircle},
    handlers::*,
    libp2p::{
        core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version, ConnectedPoint},
        futures::{self, StreamExt},
        kad::{self, QueryId},
        swarm::{NetworkBehaviour, SwarmEvent},
        Multiaddr, PeerId, Transport,
    },
    onion::{EncryptedStream, PathId},
    primitives::contracts::NodeData,
    std::{borrow::Cow, collections::HashMap, fs, io, iter, mem, time::Duration},
};

mod handlers;

compose_handlers! {
    Server {

    }
}

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
    server: Server,
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
            IDLE_TIMEOUT: u64,
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

        let (sender, receiver) = topology_wrapper::channel();
        let behaviour = Behaviour {
            onion: topology_wrapper::new(
                onion::Behaviour::new(
                    onion::Config::new(enc_keys.clone().into(), peer_id)
                        .max_streams(10)
                        .keep_alive_interval(Duration::from_secs(100)),
                ),
                sender.clone(),
            ),
            kad: topology_wrapper::new(
                kad::Behaviour::with_config(
                    peer_id,
                    Storage::new(),
                    mem::take(
                        kad::Config::default()
                            .set_replication_factor(REPLICATION_FACTOR)
                            .set_record_filtering(StoreInserts::FilterBoth),
                    ),
                ),
                sender.clone(),
            ),
            identfy: topology_wrapper::new(
                libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                    "0.1.0".into(),
                    local_key.public(),
                )),
                sender.clone(),
            ),
            rpc: topology_wrapper::new(rpc::Behaviour::default(), sender.clone()),
            report: topology_wrapper::report::new(receiver),
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
        .map(move |t, _| match t {
            futures::future::Either::Left((p, m)) => (
                p,
                StreamMuxerBox::new(topology_wrapper::muxer::Muxer::new(m, sender.clone())),
            ),
            futures::future::Either::Right((p, m)) => (
                p,
                StreamMuxerBox::new(topology_wrapper::muxer::Muxer::new(m, sender.clone())),
            ),
        })
        .boxed();
        let mut swarm = libp2p::swarm::Swarm::new(
            transport,
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(Duration::from_millis(IDLE_TIMEOUT)),
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

    fn handle_event(&mut self, event: SE) {
        match event {
            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint: ConnectedPoint::Dialer { address, .. },
                ..
            } => {
                self.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer_id, address);

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
            SwarmEvent::Behaviour(BehaviourEvent::Identfy(libp2p::identify::Event::Received {
                peer_id,
                info,
            })) => {
                for addr in info.listen_addrs {
                    self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest(to))) => {
                component_utils::handle_conn_request(to, &mut self.swarm, &mut self.peer_discovery)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(
                    &e,
                    &mut self.swarm,
                    &mut self.peer_discovery,
                ) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Rpc(rpc::Event::Request(peer, cid, body))) => {
                let Some((&prefix, rest)) = body.split_first() else {
                    log::info!("invalid rpc request");
                    return;
                };

                self.swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .disable_replication();
                let res = todo!("dispatch request");
                self.swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .enable_replication();

                if let Err(e) = res {
                    log::info!("failed to dispatch rpc request: {}", e);
                    return;
                };

                for (_, packet) in self.packets.drain() {
                    self.swarm.behaviour_mut().rpc.respond(peer, cid, &*packet);
                }
                self.dispatch_events();
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::InboundStream(
                inner,
                id,
            ))) => {
                self.clients.push(Stream { assoc: id, inner });
            }
            SwarmEvent::Behaviour(ev) => {
                let _ =
                    self.server
                        .try_handle_event(self.swarm.behaviour_mut(), ev, &mut self.packets);

                for ((rid, id), packet) in self.packets.drain() {
                    match id {
                        Ok(pid) => {
                            let resp = DispatchResponse {
                                id: rid,
                                body: Reminder(packet),
                            };
                            let Some(stream) = self.clients.iter_mut().find(|s| s.assoc == pid)
                            else {
                                log::info!("client did not stay for response");
                                continue;
                            };
                            if stream.inner.write(&resp).is_none() {
                                log::info!("client cannot process the late response");
                            }
                        }
                        Err(pid) => {
                            log::info!("sending response to peer: {:?}", self.buffer);
                            self.swarm.behaviour_mut().rpc.respond(pid, rid, &*packet);
                        }
                    }
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
                log::info!("failed to read from client: {}", e);
                return;
            }
        };

        let Some(req) = chat_logic::Request::decode(&mut req.as_slice()) else {
            log::info!("failed to decode init request: {:?}", req);
            return;
        };

        log::info!(
            "received message from client: {:?} {:?}",
            req.request_id,
            req.prefix
        );

        if let Err(e) = self.server.dispatch(
            self.swarm.behaviour_mut(),
            req,
            RequestOrigin::Client(id),
            &mut self.packets,
        ) {
            log::info!("failed to dispatch init request: {}", e);
            return;
        };

        let stream = self
            .clients
            .iter_mut()
            .find(|s| s.assoc == id)
            .expect("we just received message");
        for ((rid, _), packet) in self.packets.drain() {
            let resp = DispatchResponse {
                id: rid,
                body: Reminder(packet),
            };
            if stream.inner.write(&resp).is_none() {
                log::info!("client cannot process the response");
            }
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
                    id: request_id,
                    body: Reminder(event),
                };
                let Some(stream) = self.clients.iter_mut().find(|s| s.assoc == target) else {
                    continue;
                };
                if stream.inner.write(&resp).is_none() {
                    log::info!("clien cannot process the event");
                }
            }
        }
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

type SE = libp2p::swarm::SwarmEvent<<Behaviour as NetworkBehaviour>::ToSwarm>;

#[derive(NetworkBehaviour)]
struct Behaviour {
    onion: topology_wrapper::Behaviour<onion::Behaviour>,
    kad: topology_wrapper::Behaviour<kad::Behaviour<Storage>>,
    identfy: topology_wrapper::Behaviour<libp2p::identify::Behaviour>,
    rpc: topology_wrapper::Behaviour<rpc::Behaviour>,
    report: topology_wrapper::report::Behaviour,
}

impl From<libp2p::kad::Event> for BehaviourEvent {
    fn from(e: libp2p::kad::Event) -> Self {
        BehaviourEvent::Kad(e)
    }
}

component_utils::impl_kad_search!(Behaviour => (Storage, onion::Behaviour => onion, kad));

type Stream = component_utils::stream::AsocStream<PathId, EncryptedStream>;

pub struct Storage {
    profiles: HashMap<Identity, Profile>,
    chats: HashMap<ChatName, Chat>,

    // this is true if we are dispatching put_record
    dont_replicate: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            chats: HashMap::new(),
            dont_replicate: false,
        }
    }

    pub fn disable_replication(&mut self) {
        self.dont_replicate = true;
    }

    pub fn enable_replication(&mut self) {
        self.dont_replicate = false;
    }
}

impl libp2p::kad::store::RecordStore for Storage {
    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::ProviderRecord>>
    where
        Self: 'a;
    type RecordsIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::Record>>
    where
        Self: 'a;

    fn get(&self, _: &libp2p::kad::RecordKey) -> Option<std::borrow::Cow<'_, libp2p::kad::Record>> {
        None
    }

    fn put(&mut self, _: libp2p::kad::Record) -> libp2p::kad::store::Result<()> {
        Ok(())
    }

    fn remove(&mut self, _: &libp2p::kad::RecordKey) {}

    fn records(&self) -> Self::RecordsIter<'_> {
        iter::empty()
    }

    fn add_provider(&mut self, _: libp2p::kad::ProviderRecord) -> libp2p::kad::store::Result<()> {
        Ok(())
    }

    fn providers(&self, _: &libp2p::kad::RecordKey) -> Vec<libp2p::kad::ProviderRecord> {
        Vec::new()
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        iter::empty()
    }

    fn remove_provider(&mut self, _: &libp2p::kad::RecordKey, _: &PeerId) {}
}
