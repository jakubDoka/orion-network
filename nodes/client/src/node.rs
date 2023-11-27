use chat_logic::{ChatQ, RawRequest, RequestId, RequestInit};

use {
    crate::{BootPhase, UserKeys},
    chat_logic::{
        ChatName, FetchVault, Nonce, ProfileQ, RequestDispatch, RequestStream, SearchPeers, Server,
    },
    component_utils::{
        futures::{self},
        kad::KadPeerSearch,
        Codec, LinearMap, Reminder,
    },
    crypto::{decrypt, enc, TransmutationCircle},
    leptos::signal_prelude::*,
    libp2p::{
        core::upgrade::Version,
        futures::StreamExt,
        kad::{store::MemoryStore, GetRecordOk, PeerRecord, ProgressStep, QueryResult},
        swarm::{NetworkBehaviour, SwarmEvent},
        PeerId, Swarm, *,
    },
    onion::{EncryptedStream, PathId},
    primitives::{
        contracts::{NodeData, NodeIdentity, StoredUserIdentity},
        UserName,
    },
    rand::seq::IteratorRandom,
    std::{
        collections::{HashMap, HashSet},
        io, mem,
        task::Poll,
        time::Duration,
    },
    web_sys::wasm_bindgen::JsValue,
};

pub type MessageContent = std::rc::Rc<str>;

component_utils::protocol! { 'a:
    #[derive(Default)]
    struct Vault {
        chats: LinearMap<ChatName, ChatMeta>,
        theme: Theme,
    }

    struct ChatMeta {
        secret: crypto::SharedSecret,
        action_no: Nonce,
    }

    struct RawChatMessage<'a> {
        content: &'a str,
    }
}

pub fn try_set_color(name: &str, value: u32) -> Result<(), JsValue> {
    leptos::document()
        .body()
        .ok_or("no body")?
        .style()
        .set_property(name, &format!("#{:08x}", value))
}

pub fn try_load_color_from_style(name: &str) -> Result<u32, JsValue> {
    u32::from_str_radix(
        leptos::document()
            .body()
            .ok_or("no body")?
            .style()
            .get_property_value(name)?
            .strip_prefix('#')
            .ok_or("expected # to start the color")?,
        16,
    )
    .map_err(|e| e.to_string().into())
}

macro_rules! gen_theme {
    ($(
        $name:ident: $value:literal,
    )*) => {
        component_utils::protocol! { 'a:
            #[derive(Clone, Copy, PartialEq)]
            struct Theme {
                $(
                    $name: u32,
                )*
            }
        }

        impl Theme {
            pub fn apply(self) -> Result<(), JsValue> {
                $(try_set_color(concat!("--", stringify!($name), "-color"), self.$name)?;)*
                Ok(())
            }

            pub fn from_current() -> Result<Self, JsValue> {
                Ok(Self { $(
                    $name: try_load_color_from_style(concat!("--", stringify!($name), "-color"))?,
                )* })
            }

            pub const KEYS: &'static [&'static str] = &[$(stringify!($name),)*];
        }

        impl Default for Theme {
            fn default() -> Self {
                Self { $( $name: $value,)* }
            }
        }
    };
}

gen_theme! {
    primary: 0x000000ff,
    secondary: 0x333333ff,
    highlight: 0xffffffff,
    font: 0xffffffff,
    error: 0xff0000ff,
}

pub struct Node {
    keys: UserKeys,
    swarm: Swarm<Behaviour>,
    peer_search: KadPeerSearch,
    subscriptions: futures::stream::SelectAll<Subscription>,
    pending_requests: Vec<(
        RequestId,
        libp2p::futures::channel::oneshot::Sender<RawRequest>,
    )>,
    nodes: HashMap<PeerId, enc::PublicKey>,
    requests: RequestStream,
    vault: Vault,
    profile_nonce: Nonce,
}

impl Node {
    pub async fn new(
        keys: UserKeys,
        wboot_phase: WriteSignal<Option<BootPhase>>,
    ) -> Result<(Self, RequestDispatch<Server>), BootError> {
        wboot_phase(Some(BootPhase::FetchTopology));

        let mut peer_search = KadPeerSearch::default();
        let (mut request_dispatch, commands) = RequestDispatch::new();
        let chain_api = crate::chain_node(keys.name).await.unwrap();
        let node_request = chain_api.list(crate::node_contract());
        let profile_request = chain_api.get_profile_by_name(crate::user_contract(), keys.name);
        let (node_data, profile_hash) = futures::join!(node_request, profile_request);
        let (node_data, profile_hash) = (
            node_data.map_err(BootError::FetchNodes)?,
            profile_hash
                .map_err(BootError::FetchProfile)?
                .ok_or(BootError::ProfileNotFound)?,
        );
        let profile = keys.to_identity();
        let profile_hash = StoredUserIdentity::from_bytes(profile_hash);
        if profile_hash.verify(&profile) {
            return Err(BootError::InvalidProfileKeys);
        }

        if node_data.len() < MIN_NODES {
            return Err(BootError::NotEnoughNodes(node_data.len()));
        }

        wboot_phase(Some(BootPhase::InitiateConnection));
        let keypair = identity::Keypair::generate_ed25519();
        let transport = websocket_websys::Transport::new(100)
            .upgrade(Version::V1)
            .authenticate(noise::Config::new(&keypair).unwrap())
            .multiplex(yamux::Config::default())
            .boxed();
        let peer_id = keypair.public().to_peer_id();

        let behaviour = Behaviour {
            onion: onion::Behaviour::new(
                onion::Config::new(None, peer_id).keep_alive_interval(Duration::from_secs(100)),
            ),
            kad: kad::Behaviour::with_config(
                peer_id,
                kad::store::MemoryStore::new(peer_id),
                mem::take(
                    kad::Config::default().set_replication_factor(chat_logic::REPLICATION_FACTOR),
                ),
            ),
            identify: identify::Behaviour::new(identify::Config::new("l".into(), keypair.public())),
        };

        let mut swarm = swarm::Swarm::new(
            transport,
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_wasm_executor().with_idle_connection_timeout(Duration::MAX), // TODO: please, dont
        );
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));

        swarm.dial(crate::boot_node()).unwrap();
        loop {
            match swarm.select_next_some().await {
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                SwarmEvent::ConnectionEstablished { .. } => {
                    break;
                }
                _ => {}
            }
        }
        let qid = swarm.behaviour_mut().kad.bootstrap().unwrap();
        loop {
            match swarm.select_next_some().await {
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                SwarmEvent::Behaviour(BehaviourEvent::Kad(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: QueryResult::Bootstrap(_),
                        step: ProgressStep { last: true, .. },
                        ..
                    },
                )) if id == qid => break,
                _ => {}
            }
        }

        let mut query_pool = node_data
            .iter()
            .map(|nd| nd.sign.0.to_vec().into())
            .map(|id| swarm.behaviour_mut().kad.get_record(id))
            .zip(node_data.iter())
            .collect::<Vec<_>>();
        let mut nodes = HashMap::new();
        while !node_data.is_empty() {
            let (result, id) = match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Kad(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: QueryResult::GetRecord(result),
                        step: ProgressStep { last: true, .. },
                        ..
                    },
                )) => (result, id),
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => continue,
                _ => continue,
            };

            let Some(index) = query_pool.iter().position(|q| q.0 == id) else {
                continue;
            };
            let (.., nd) = query_pool.swap_remove(index);

            let Ok(GetRecordOk::FoundRecord(PeerRecord { record, .. })) = result else {
                continue;
            };

            let Some(identity) = NodeIdentity::try_from_slice(&record.value) else {
                continue;
            };

            let pk: libp2p::identity::PublicKey =
                libp2p::identity::ed25519::PublicKey::try_from_bytes(&nd.id)
                    .unwrap()
                    .into();

            nodes.insert(pk.to_peer_id(), identity.enc);
        }

        let route = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .take(3)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        wboot_phase(Some(BootPhase::InitialRoute));

        let pid = swarm.behaviour_mut().onion.open_path(route).unwrap();
        let (mut init_stream, init_stream_id, init_stream_per) = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                    peer,
                ))) if id == pid => break (stream, id, peer),
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                e => log::error!("{:?}", e),
            }
        };

        wboot_phase(Some(BootPhase::ProfileSearch));

        let members = request_dispatch
            .dispatch_direct::<SearchPeers<ProfileQ>>(&mut init_stream, &profile_hash.sign)
            .await
            .unwrap();

        wboot_phase(Some(BootPhase::ProfileLoad));

        let (mut profile_stream, profile_stream_id, profile_stream_peer) =
            if members.contains(&init_stream_per) {
                (init_stream, init_stream_id, init_stream_per)
            } else {
                let Some(pick) = members.into_iter().choose(&mut rand::thread_rng()) else {
                    todo!("error handling")
                };
                let route = pick_route(&nodes, pick);
                let pid = swarm.behaviour_mut().onion.open_path(route).unwrap();
                loop {
                    match swarm.select_next_some().await {
                        SwarmEvent::Behaviour(BehaviourEvent::Onion(
                            onion::Event::OutboundStream(stream, id, _),
                        )) if id == pid => break (stream, id, pick),
                        e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                        e => log::error!("{:?}", e),
                    }
                }
            };

        let (profile_nonce, Reminder(vault)) = request_dispatch
            .dispatch_direct::<FetchVault>(&mut profile_stream, &profile_hash.sign)
            .await
            .unwrap()
            .unwrap();

        let vault = if vault.is_empty() {
            Default::default()
        } else {
            // not ideal
            let mut vault = vault.to_vec();
            let vault = decrypt(&mut vault, keys.vault).unwrap();
            Vault::decode(&mut &*vault).unwrap()
        };

        let _ = vault.theme.apply();

        wboot_phase(Some(BootPhase::ChatSearch));

        let mut profile_sub = Subscription {
            id: profile_stream_id,
            peer_id: profile_stream_peer,
            topics: [profile_hash.sign.0.to_vec()].into(),
            stream: profile_stream,
        };

        let mut topology = HashMap::<PeerId, HashSet<ChatName>>::new();
        let mut discovered = 0;
        for (peers, chat) in request_dispatch
            .dispatch_direct_batch::<SearchPeers<ChatQ>>(
                &mut profile_sub.stream,
                vault.chats.keys().copied(),
            )
            .await
            .unwrap()
        {
            if peers.contains(&profile_sub.peer_id) {
                profile_sub.topics.insert(chat.to_bytes());
                continue;
            }

            for peer in peers {
                topology.entry(peer).or_default().insert(chat);
            }
            discovered += 1;
        }

        wboot_phase(Some(BootPhase::ChatLoad));

        let mut topology = topology.into_iter().collect::<Vec<_>>();
        topology.sort_by_key(|(_, v)| v.len());
        let mut to_connect = vec![];
        let mut seen = HashSet::new();
        while seen.len() < discovered {
            let (peer, mut chats) = topology.pop().unwrap();
            chats.retain(|&c| seen.insert(c));
            if chats.is_empty() {
                continue;
            }
            to_connect.push((peer, chats));
        }

        let mut awaiting = to_connect
            .into_iter()
            .map(|(pick, set)| {
                let route = pick_route(&nodes, pick);
                let pid = swarm.behaviour_mut().onion.open_path(route).unwrap();
                (pid, pick, set)
            })
            .collect::<Vec<_>>();

        let mut subscriptions = futures::stream::SelectAll::new();
        while !awaiting.is_empty() {
            let (mut stream, subs, peer_id, id) =
                loop {
                    match swarm.select_next_some().await {
                        SwarmEvent::Behaviour(BehaviourEvent::Onion(
                            onion::Event::OutboundStream(stream, id, pid),
                        )) => {
                            if let Some(i) = awaiting.iter().position(|&(i, ..)| i == id) {
                                let (.., peer_id, subs) = awaiting.swap_remove(i);
                                debug_assert!(peer_id == pid);
                                break (stream, subs, peer_id, id);
                            }
                        }
                        e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                        e => log::debug!("{:?}", e),
                    }
                };

            subscriptions.push(Subscription {
                id,
                peer_id,
                topics: subs.into_iter().map(|c| c.as_bytes().to_vec()).collect(),
                stream,
            });
        }

        wboot_phase(Some(BootPhase::ChatRun));

        Ok((
            Self {
                keys,
                swarm,
                nodes,
                peer_search,
                subscriptions,
                requests: commands,
                vault,
                profile_nonce,
            },
            request_dispatch,
        ))
    }

    fn try_handle_common_event(
        ev: &SE,
        swarm: &mut Swarm<Behaviour>,
        peer_search: &mut KadPeerSearch,
    ) -> bool {
        match ev {
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
            })) => {
                if let Some(addr) = info.listen_addrs.first() {
                    swarm.behaviour_mut().kad.add_address(peer_id, addr.clone());
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest(to))) => {
                component_utils::handle_conn_request(*to, swarm, peer_search);
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(e, swarm, peer_search) => {}
            _ => return false,
        }

        true
    }

    pub async fn run(mut self) {
        loop {
            futures::select! {
                event = self.swarm.select_next_some() => self.handle_swarm_event(event),
                command = self.requests.select_next_some() => self.handle_command(command),
                (id, response) = self.subscriptions.select_next_some() => self.handle_subscription_response(id, response),
            }
        }
    }

    fn handle_mail(&mut self, bytes: &[u8]) {
        todo!()
    }

    fn handle_request(&mut self, req: RawRequest) {}

    fn handle_command(&mut self, command: RequestInit) {
        match command {
            RequestInit::Request(req) => self.handle_request(req),
            RequestInit::Subscription(sub) => self.handle_subscription(sub),
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                mut stream,
                id,
                peer_id,
            ))) => {
                todo!()
            }
            e if Self::try_handle_common_event(&e, &mut self.swarm, &mut self.peer_search) => {}
            e => log::debug!("{:?}", e),
        }
    }

    fn handle_subscription_response(&mut self, id: PathId, request: io::Result<Vec<u8>>) {
        let Ok(msg) = request.inspect_err(|e| log::error!("chat subscription error: {e}")) else {
            return;
        };

        todo!()
    }

    pub fn username(&self) -> UserName {
        self.keys.name
    }

    pub fn chats(&self) -> impl Iterator<Item = ChatName> + '_ {
        self.vault.chats.keys().copied()
    }
}

fn pick_route(
    nodes: &HashMap<PeerId, enc::PublicKey>,
    target: PeerId,
) -> [(onion::PublicKey, PeerId); 3] {
    assert!(nodes.len() >= 2);
    let mut rng = rand::thread_rng();
    let mut picked = nodes
        .iter()
        .filter(|(p, _)| **p != target)
        .map(|(p, ud)| (*ud, *p))
        .choose_multiple(&mut rng, 2);
    picked.insert(0, (*nodes.get(&target).unwrap(), target));
    picked.try_into().unwrap()
}

pub fn send_request<'a, T: Codec<'a>>(resp: T, stream: &mut EncryptedStream, buffer: &mut Vec<u8>) {
    buffer.clear();
    resp.encode(buffer);
    stream.write(buffer);
}

#[allow(deprecated)]
type SE = libp2p::swarm::SwarmEvent<<Behaviour as NetworkBehaviour>::ToSwarm>;

const MIN_NODES: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("not enough nodes: {0} < {MIN_NODES}")]
    NotEnoughNodes(usize),
    #[error(transparent)]
    Encapsulation(#[from] crypto::enc::EncapsulationError),
    #[error("failed to fetch nodes: {0}")]
    FetchNodes(chain_api::Error),
    #[error("failed to fetch profile: {0}")]
    FetchProfile(chain_api::Error),
    #[error("profile not found")]
    ProfileNotFound,
    #[error("invalid profile keys")]
    InvalidProfileKeys,
    #[error("failed to search for chat (stream broken): {0}")]
    ChatSearch(io::Error),
}

fn node_data_to_path_seg(data: NodeData) -> (PeerId, NodeData) {
    let peer_id = component_utils::libp2p::identity::PublicKey::from(
        component_utils::libp2p::identity::ed25519::PublicKey::try_from_bytes(
            &crypto::sign::PublicKey::from(data.sign).ed,
        )
        .unwrap(),
    )
    .to_peer_id();
    (peer_id, data)
}

struct Subscription {
    id: PathId,
    peer_id: PeerId,
    topics: HashSet<Vec<u8>>,
    stream: EncryptedStream,
}

impl futures::Stream for Subscription {
    type Item = (PathId, <EncryptedStream as futures::Stream>::Item);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.stream
            .poll_next_unpin(cx)
            .map(|opt| opt.map(|v| (self.id, v)))
    }
}

#[derive(libp2p::swarm::NetworkBehaviour)]
struct Behaviour {
    onion: onion::Behaviour,
    kad: libp2p::kad::Behaviour<MemoryStore>,
    identify: libp2p::identify::Behaviour,
}

component_utils::impl_kad_search!(Behaviour => (onion::Behaviour => onion));
