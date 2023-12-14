#![feature(iter_advance_by)]
#![feature(iter_next_chunk)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]
#![feature(macro_metavar_expr)]

use {
    self::handlers::RequestOrigin,
    anyhow::Context as _,
    chain_api::ContractId,
    chat_logic::*,
    component_utils::{kad::KadPeerSearch, libp2p::kad::StoreInserts, Codec, LinearMap, Reminder},
    crypto::{enc, sign, TransmutationCircle},
    handlers::{Repl, *},
    libp2p::{
        core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version, ConnectedPoint},
        futures::{self, stream::SelectAll, StreamExt},
        kad::{self, QueryId},
        swarm::{NetworkBehaviour, SwarmEvent},
        Multiaddr, PeerId, Transport,
    },
    onion::{EncryptedStream, PathId},
    primitives::contracts::NodeData,
    std::{
        borrow::Cow, collections::HashMap, convert::Infallible, fs, io, iter, mem, time::Duration,
    },
};

macro_rules! extract_ctx {
    ($self:expr) => {
        Context {
            swarm: &mut $self.swarm,
            streams: &mut $self.clients,
        }
    };
}

mod handlers;

compose_handlers! {
    InternalServer {
        Sync<CreateProfile>, Sync<SetVault>, Sync<SendMail>, Sync<ReadMail>, Sync<FetchProfile>,
        Sync<CreateChat>, Sync<AddUser>, Sync<SendMessage>,
    }

    ExternalServer {
        handlers::SearchPeers,
        Sync<Subscribe>,

        Repl<CreateProfile>, Repl<SetVault>, Repl<SendMail>, Repl<ReadMail>, Repl<FetchProfile>,
        Sync<FetchVault>,
        Repl<CreateChat>, Repl<AddUser>, Repl<SendMessage>,
        Sync<FetchMessages>,
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
    internal: InternalServer,
    external: ExternalServer,
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
            internal: Default::default(),
            external: Default::default(),
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
            SwarmEvent::Behaviour(BehaviourEvent::Rpc(rpc::Event::Request(peer, id, body))) => {
                let Some((&prefix, body)) = body.split_first() else {
                    log::info!("invalid rpc request");
                    return;
                };

                let req = handlers::Request {
                    prefix,
                    id,
                    origin: RequestOrigin::Miner(peer),
                    body,
                };
                self.buffer.clear();
                let res = self
                    .internal
                    .execute(&mut extract_ctx!(self), req, &mut self.buffer);
                match res {
                    Ok(false) => {}
                    Ok(true) => {
                        self.swarm
                            .behaviour_mut()
                            .rpc
                            .respond(peer, id, self.buffer.as_slice());
                    }
                    Err(e) => {
                        log::info!("failed to dispatch rpc request: {}", e);
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::InboundStream(
                inner,
                id,
            ))) => {
                self.clients.push(Stream::new(id, inner));
            }
            SwarmEvent::Behaviour(ev) => {
                self.buffer.clear();
                let cx = &mut extract_ctx!(self);
                let Ok((origin, id)) = Err(ev)
                    .or_else(|ev| self.internal.try_complete(cx, ev, &mut self.buffer))
                    .or_else(|ev| self.external.try_complete(cx, ev, &mut self.buffer))
                else {
                    return;
                };

                match origin {
                    RequestOrigin::Client(pid) => {
                        let Some(stream) = self.clients.iter_mut().find(|s| s.id == pid) else {
                            log::info!("client did not stay for response");
                            return;
                        };
                        if stream.inner.write((id, Reminder(&self.buffer))).is_none() {
                            log::info!("client cannot process the late response");
                        }
                    }
                    RequestOrigin::Miner(mid) => {
                        self.swarm
                            .behaviour_mut()
                            .rpc
                            .respond(mid, id, self.buffer.as_slice());
                    }
                    RequestOrigin::NotImportant => {}
                }
            }
            e => log::debug!("{e:?}"),
        }
    }

    fn handle_client_message(&mut self, id: PathId, req: io::Result<Vec<u8>>) {
        let req = match req {
            Ok(req) => req,
            Err(e) => {
                log::info!("failed to read from client: {}", e);
                return;
            }
        };

        let Some(req) = chat_logic::Request::decode(&mut req.as_slice()) else {
            log::info!("failed to decode client request: {:?}", req);
            return;
        };

        log::info!(
            "received message from client: {:?} {:?}",
            req.id,
            req.prefix
        );

        let req = handlers::Request {
            prefix: req.prefix,
            id: req.id,
            origin: RequestOrigin::Client(id),
            body: req.body.0,
        };
        self.buffer.clear();
        let res = self
            .external
            .execute(&mut extract_ctx!(self), req, &mut self.buffer);

        match res {
            Ok(false) => {}
            Ok(true) => {
                let stream = self
                    .clients
                    .iter_mut()
                    .find(|s| s.id == id)
                    .expect("we just received message");
                if stream
                    .inner
                    .write((req.id, Reminder(&self.buffer)))
                    .is_none()
                {
                    log::info!("client cannot process the response");
                }
            }
            Err(e) => {
                log::info!("failed to dispatch client request request: {}", e);
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

struct Context<'a> {
    swarm: &'a mut libp2p::swarm::Swarm<Behaviour>,
    streams: &'a mut SelectAll<Stream>,
}

impl ProvideSubscription for Context<'_> {
    fn subscribe(&mut self, topic: PossibleTopic, id: CallId, origin: PathId) {
        let Some(stream) = self.streams.iter_mut().find(|s| s.id == origin) else {
            log::error!("whaaaat???");
            return;
        };
        stream.subscriptions.insert(topic, id);
    }
}

impl ProvideKad for Context<'_> {
    fn kad_mut(
        &mut self,
    ) -> &mut libp2p::kad::Behaviour<impl kad::store::RecordStore + Send + 'static> {
        &mut self.swarm.behaviour_mut().kad
    }
}

impl ProvideStorage for Context<'_> {
    fn store_mut(&mut self) -> &mut crate::Storage {
        self.swarm.behaviour_mut().kad.store_mut()
    }
}

impl ProvideRpc for Context<'_> {
    fn rpc_mut(&mut self) -> &mut rpc::Behaviour {
        &mut self.swarm.behaviour_mut().rpc
    }
}

impl EventEmmiter<ChatName> for Context<'_> {
    fn push(&mut self, topic: ChatName, event: <ChatName as Topic>::Event<'_>) {
        handle_event(self.streams, PossibleTopic::Chat(topic), event);
    }
}

impl EventEmmiter<Identity> for Context<'_> {
    fn push(&mut self, topic: Identity, event: <Identity as Topic>::Event<'_>) {
        handle_event(self.streams, PossibleTopic::Profile(topic), event);
    }
}

fn handle_event<'a>(streams: &mut SelectAll<Stream>, topic: PossibleTopic, event: impl Codec<'a>) {
    for stream in streams.iter_mut() {
        let Some(&call_id) = stream.subscriptions.get(&topic) else {
            continue;
        };

        if stream.inner.write((call_id, &event)).is_none() {
            log::info!("client cannot process the subscription response");
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

impl From<Infallible> for BehaviourEvent {
    fn from(v: Infallible) -> Self {
        match v {}
    }
}

impl TryUnwrap<Infallible> for BehaviourEvent {
    fn try_unwrap(self) -> Result<Infallible, Self> {
        Err(self)
    }
}

impl From<libp2p::kad::Event> for BehaviourEvent {
    fn from(v: libp2p::kad::Event) -> Self {
        BehaviourEvent::Kad(v)
    }
}

impl TryUnwrap<kad::Event> for BehaviourEvent {
    fn try_unwrap(self) -> Result<kad::Event, Self> {
        match self {
            BehaviourEvent::Kad(e) => Ok(e),
            e => Err(e),
        }
    }
}

impl From<ReplEvent> for BehaviourEvent {
    fn from(v: ReplEvent) -> Self {
        match v {
            ReplEvent::Kad(e) => BehaviourEvent::Kad(e),
            ReplEvent::Rpc(e) => BehaviourEvent::Rpc(e),
        }
    }
}

impl TryUnwrap<ReplEvent> for BehaviourEvent {
    fn try_unwrap(self) -> Result<ReplEvent, Self> {
        match self {
            BehaviourEvent::Kad(e) => Ok(ReplEvent::Kad(e)),
            BehaviourEvent::Rpc(e) => Ok(ReplEvent::Rpc(e)),
            e => Err(e),
        }
    }
}

component_utils::impl_kad_search!(Behaviour => (Storage, onion::Behaviour => onion, kad));

pub struct Stream {
    id: PathId,
    subscriptions: LinearMap<PossibleTopic, CallId>,
    inner: EncryptedStream,
}

impl Stream {
    fn new(id: PathId, inner: EncryptedStream) -> Stream {
        Stream {
            id,
            subscriptions: Default::default(),
            inner,
        }
    }
}

impl libp2p::futures::Stream for Stream {
    type Item = (PathId, io::Result<Vec<u8>>);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner
            .poll_next_unpin(cx)
            .map(|v| v.map(|v| (self.id, v)))
    }
}

pub struct Storage {
    profiles: HashMap<Identity, Profile>,
    chats: HashMap<ChatName, Chat>,
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
        }
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
