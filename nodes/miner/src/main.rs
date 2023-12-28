#![feature(iter_advance_by)]
#![feature(iter_next_chunk)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]
#![feature(macro_metavar_expr)]

use {
    self::handlers::RequestOrigin,
    anyhow::Context as _,
    chain_api::{ContractId, NodeAddress},
    chat_logic::*,
    component_utils::{kad::KadPeerSearch, libp2p::kad::StoreInserts, Codec, LinearMap, Reminder},
    crypto::{enc, sign, TransmutationCircle},
    handlers::{Repl, SendMail, SyncRepl, *},
    libp2p::{
        core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version, ConnectedPoint},
        futures::{self, stream::SelectAll, SinkExt, StreamExt},
        kad::{self, QueryId},
        swarm::{NetworkBehaviour, SwarmEvent},
        Multiaddr, PeerId, Transport,
    },
    onion::{EncryptedStream, PathId},
    primitives::contracts::{NodeData, StoredNodeData},
    std::{
        borrow::Cow,
        collections::HashMap,
        convert::Infallible,
        fs, io, iter, mem,
        net::{IpAddr, Ipv4Addr},
        time::Duration,
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
#[cfg(test)]
mod tests;
mod white_list;

compose_handlers! {
    InternalServer {
        Sync<CreateProfile>, Sync<SetVault>, SendMail, Sync<ReadMail>, Sync<FetchProfile>,
        Sync<CreateChat>, Sync<AddUser>, Sync<SendMessage>,
    }

    ExternalServer {
        Sync<SearchPeers>,
        Sync<Subscribe>,

        SyncRepl<CreateProfile>, SyncRepl<SetVault>, Repl<SendMail>, SyncRepl<ReadMail>, SyncRepl<FetchProfile>,
        Sync<FetchVault>,
        SyncRepl<CreateChat>, SyncRepl<AddUser>, SyncRepl<SendMessage>,
        Sync<FetchMessages>,
    }
}

#[derive(Default, Clone)]
struct NodeKeys {
    enc: enc::Keypair,
    sign: sign::KeyPair,
}

crypto::impl_transmute! {
    NodeKeys,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let node_config = NodeConfig::from_env();
    let chain_config = ChainConfig::from_env();
    let (keys, is_new) = Miner::load_keys(&node_config.key_path)?;
    let (node_list, stake_events) = deal_with_chain(chain_config, &keys, is_new).await?;

    Miner::new(node_config, keys, node_list, stake_events)
        .await?
        .run()
        .await;

    Ok(())
}

config::env_config! {
    struct NodeConfig {
        port: u16,
        ws_port: u16,
        key_path: String,
        boot_nodes: config::List<Multiaddr>,
        idle_timeout: u64,
    }
}

config::env_config! {
    struct ChainConfig {
        exposed_address: IpAddr,
        port: u16,
        nonce: u64,
        chain_node: String,
        node_account: String,
        node_contract: ContractId,
    }
}

type StakeEvents = futures::channel::mpsc::Receiver<chain_api::Result<chain_api::StakeEvent>>;

struct Miner {
    swarm: libp2p::swarm::Swarm<Behaviour>,
    peer_discovery: KadPeerSearch,
    clients: futures::stream::SelectAll<Stream>,
    buffer: Vec<u8>,
    bootstrapped: Option<QueryId>,
    internal: InternalServer,
    external: ExternalServer,
    stake_events: StakeEvents,
}

fn unpack_node_id(id: sign::Ed) -> anyhow::Result<PeerId> {
    let peer_id = {
        let ed_kp = libp2p::identity::ed25519::PublicKey::try_from_bytes(&id)
            .context("deriving ed signature")?;
        libp2p::identity::PublicKey::from(ed_kp).to_peer_id()
    };
    Ok(peer_id)
}

fn unpack_node_addr(addr: chain_api::NodeAddress) -> Multiaddr {
    let (addr, port) = addr.into();
    Multiaddr::empty()
        .with(match addr {
            IpAddr::V4(ip) => multiaddr::Protocol::Ip4(ip),
            IpAddr::V6(ip) => multiaddr::Protocol::Ip6(ip),
        })
        .with(multiaddr::Protocol::Tcp(port))
}

async fn deal_with_chain(
    config: ChainConfig,
    keys: &NodeKeys,
    is_new: bool,
) -> anyhow::Result<(Vec<(StoredNodeData, NodeAddress)>, StakeEvents)> {
    let ChainConfig {
        chain_node,
        node_account,
        node_contract,
        port,
        exposed_address,
        nonce,
    } = config;
    let (mut chain_events_tx, stake_events) = futures::channel::mpsc::channel(0);
    let account = if node_account.starts_with("//") {
        chain_api::dev_keypair(&node_account)
    } else {
        chain_api::mnemonic_keypair(&node_account)
    };

    let client = chain_api::Client::with_signer(&chain_node, account)
        .await
        .context("connecting to chain")?;

    let node_list = client
        .list(node_contract.clone())
        .await
        .context("fetching node list")?;

    let stream = client
        .node_contract_event_stream(node_contract.clone())
        .await?;
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(stream);
        // TODO: properly recover fro errors, add node pool to try
        while let Some(event) = stream.next().await {
            _ = chain_events_tx.send(event).await;
        }
    });

    if is_new {
        client
            .join(
                node_contract.clone(),
                NodeData {
                    sign: keys.sign.public_key(),
                    enc: keys.enc.public_key(),
                }
                .to_stored(),
                (exposed_address, port).into(),
                nonce,
            )
            .await
            .context("registeing to chain")?;
        log::info!("registered on chain");
    }

    log::info!("entered the network with {} nodes", node_list.len());

    Ok((node_list, stake_events))
}

impl Miner {
    async fn new(
        config: NodeConfig,
        keys: NodeKeys,
        node_list: Vec<(StoredNodeData, NodeAddress)>,
        stake_events: StakeEvents,
    ) -> anyhow::Result<Self> {
        let NodeConfig {
            port,
            ws_port,
            boot_nodes,
            idle_timeout,
            ..
        } = config;

        let local_key = libp2p::identity::Keypair::ed25519_from_bytes(keys.sign.pre_quantum())
            .context("deriving ed signature")?;
        let peer_id = local_key.public().to_peer_id();
        log::info!("peer id: {}", peer_id);

        let (sender, receiver) = topology_wrapper::channel();
        let behaviour = Behaviour {
            white_list: white_list::Behaviour::new(port + 100),
            onion: topology_wrapper::new(
                onion::Behaviour::new(
                    onion::Config::new(keys.enc.clone().into(), peer_id)
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
                .with_idle_connection_timeout(Duration::from_millis(idle_timeout)),
        );

        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4(Ipv4Addr::UNSPECIFIED))
                    .with(multiaddr::Protocol::Tcp(port)),
            )
            .context("starting to listen for peers")?;

        swarm
            .listen_on(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4(Ipv4Addr::UNSPECIFIED))
                    .with(multiaddr::Protocol::Tcp(ws_port))
                    .with(multiaddr::Protocol::Ws("/".into())),
            )
            .context("starting to isten for clients")?;

        tokio::time::sleep(Duration::from_secs(1)).await;

        for (node, addr) in node_list {
            let peer_id = unpack_node_id(node.id)?;
            swarm
                .behaviour_mut()
                .kad
                .add_address(&peer_id, unpack_node_addr(addr));
            swarm.behaviour_mut().white_list.allow_peer(peer_id);
        }

        for boot_node in boot_nodes.0 {
            swarm.dial(boot_node).context("dialing a boot peer")?;
        }

        Ok(Self {
            swarm,
            peer_discovery: Default::default(),
            clients: Default::default(),
            buffer: Default::default(),
            bootstrapped: None,
            stake_events,
            internal: Default::default(),
            external: Default::default(),
        })
    }

    fn load_keys(path: &str) -> io::Result<(NodeKeys, bool)> {
        let file = match fs::read(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let nk = NodeKeys::default();
                fs::write(path, nk.as_bytes())?;
                return Ok((nk, true));
            }
            Err(e) => return Err(e),
        };

        let Some(nk) = NodeKeys::try_from_slice(&file).cloned() else {
            return Err(io::Error::other("invalid key file"));
        };

        Ok((nk, false))
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
                if !self.swarm.behaviour_mut().white_list.is_allowed(&peer) {
                    log::warn!(
                        "peer {} made rpc request but is not on the white list",
                        peer
                    );
                    return;
                }

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

    fn handle_stake_event(&mut self, event: Result<chain_api::StakeEvent, chain_api::Error>) {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                log::info!("failed to read from chain: {}", e);
                return;
            }
        };

        match event {
            chain_api::StakeEvent::Joined(j) => {
                let Ok(peer_id) = unpack_node_id(j.identity) else {
                    log::info!("invalid node id");
                    return;
                };
                self.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer_id, unpack_node_addr(j.addr));
                self.swarm.behaviour_mut().white_list.allow_peer(peer_id);
            }
            chain_api::StakeEvent::Reclaimed(r) => {
                let Ok(peer_id) = unpack_node_id(r.identity) else {
                    log::info!("invalid node id");
                    return;
                };
                self.swarm.behaviour_mut().kad.remove_peer(&peer_id);
                self.swarm.behaviour_mut().white_list.disallow_peer(peer_id);
            }
            chain_api::StakeEvent::AddrChanged(c) => {
                let Ok(peer_id) = unpack_node_id(c.identity) else {
                    log::info!("invalid node id");
                    return;
                };
                self.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer_id, unpack_node_addr(c.addr));
            }
        }
    }

    async fn run(mut self) {
        loop {
            futures::select! {
                e = self.swarm.select_next_some() => self.handle_event(e),
                e = self.stake_events.select_next_some() => self.handle_stake_event(e),
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

impl ProvidePeerId for Context<'_> {
    fn peer_id(&self) -> PeerId {
        *self.swarm.local_peer_id()
    }
}

impl ProvideKadAndRpc for Context<'_> {
    fn kad_and_rpc_mut(
        &mut self,
    ) -> (
        &mut libp2p::kad::Behaviour<impl kad::store::RecordStore + Send + 'static>,
        &mut rpc::Behaviour,
    ) {
        let beh = self.swarm.behaviour_mut();
        (&mut beh.kad, &mut beh.rpc)
    }
}

impl ProvideStorage for Context<'_> {
    fn store_mut(&mut self) -> &mut crate::Storage {
        self.swarm.behaviour_mut().kad.store_mut()
    }
}

impl EventEmmiter<ChatName> for Context<'_> {
    fn push(&mut self, topic: ChatName, event: <ChatName as Topic>::Event<'_>) {
        handle_event(self.streams, PossibleTopic::Chat(topic), event)
    }
}

impl DirectedEventEmmiter<Identity> for Context<'_> {
    fn push(
        &mut self,
        topic: Identity,
        event: <Identity as Topic>::Event<'_>,
        recip: PathId,
    ) -> bool {
        let Some(stream) = self.streams.iter_mut().find(|s| s.id == recip) else {
            return false;
        };

        let Some(&call_id) = stream.subscriptions.get(&PossibleTopic::Profile(topic)) else {
            return false;
        };

        if stream.inner.write((call_id, &event)).is_none() {
            return false;
        }

        true
    }
}

fn handle_event<'a>(streams: &mut SelectAll<Stream>, topic: PossibleTopic, event: impl Codec<'a>) {
    for stream in streams.iter_mut() {
        let Some(&call_id) = stream.subscriptions.get(&topic) else {
            continue;
        };

        if stream.inner.write((call_id, &event)).is_none() {
            log::info!("client cannot process the subscription response");
            continue;
        }
    }
}

type SE = libp2p::swarm::SwarmEvent<<Behaviour as NetworkBehaviour>::ToSwarm>;

#[derive(NetworkBehaviour)]
struct Behaviour {
    white_list: white_list::Behaviour,
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

impl From<rpc::Event> for BehaviourEvent {
    fn from(v: rpc::Event) -> Self {
        BehaviourEvent::Rpc(v)
    }
}

impl TryUnwrap<rpc::Event> for BehaviourEvent {
    fn try_unwrap(self) -> Result<rpc::Event, Self> {
        match self {
            BehaviourEvent::Rpc(e) => Ok(e),
            e => Err(e),
        }
    }
}

impl<'a> TryUnwrap<&'a rpc::Event> for &'a Infallible {
    fn try_unwrap(self) -> Result<&'a rpc::Event, &'a Infallible> {
        Err(self)
    }
}

impl<'a> TryUnwrap<&'a Infallible> for &'a rpc::Event {
    fn try_unwrap(self) -> Result<&'a Infallible, &'a rpc::Event> {
        Err(self)
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
