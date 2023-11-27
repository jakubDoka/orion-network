use {
    crate::{BootPhase, UserKeys},
    chat_logic::{
        ChatName, CreateAccount, DispatchResponse, FetchVault, Nonce, Proof, RawRequest,
        RawResponse, RequestDispatch, RequestId, RequestInit, RequestStream, SearchPeers, Server,
        SubscriptionInit, SubscriptionMessage,
    },
    component_utils::{
        futures::{self},
        kad::KadPeerSearch,
        Codec, LinearMap, Reminder,
    },
    crypto::{decrypt, enc, sign, TransmutationCircle},
    leptos::signal_prelude::*,
    libp2p::{
        core::{upgrade::Version, ConnectedPoint},
        futures::{SinkExt, StreamExt},
        kad::{store::MemoryStore, GetRecordOk, PeerRecord, ProgressStep, QueryResult},
        swarm::{NetworkBehaviour, SwarmEvent},
        PeerId, Swarm, *,
    },
    onion::{EncryptedStream, PathId},
    primitives::{
        contracts::{NodeIdentity, StoredUserIdentity},
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
        sender: UserName,
        content: &'a str,
    }
}

impl ChatMeta {
    pub fn new() -> Self {
        Self {
            secret: crypto::new_secret(),
            action_no: 1,
        }
    }
}

impl Vault {
    pub fn next_chat_proof(&mut self, chat: ChatName, kp: &sign::KeyPair) -> Option<Proof> {
        let meta = self.chats.get_mut(&chat)?;
        Some(Proof::for_chat(kp, &mut meta.action_no, chat))
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
    swarm: Swarm<Behaviour>,
    peer_search: KadPeerSearch,
    subscriptions: futures::stream::SelectAll<Subscription>,
    pending_requests: Vec<(
        RequestId,
        libp2p::futures::channel::oneshot::Sender<RawResponse>,
    )>,
    pending_topic_search: Vec<(Result<RequestId, PathId>, RequestInit)>,
    active_subs: Vec<(
        RequestId,
        libp2p::futures::channel::mpsc::Sender<SubscriptionMessage>,
    )>,
    nodes: HashMap<PeerId, enc::PublicKey>,
    requests: RequestStream,
}

impl Node {
    pub async fn new(
        keys: UserKeys,
        wboot_phase: WriteSignal<Option<BootPhase>>,
    ) -> Result<(Self, Vault, RequestDispatch<Server>, Nonce), BootError> {
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
        if !profile_hash.verify(&profile) {
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
                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint: ConnectedPoint::Dialer { address, .. },
                    ..
                } => {
                    swarm.behaviour_mut().kad.add_address(&peer_id, address);
                    break;
                }
                _ => {}
            }
        }

        wboot_phase(Some(BootPhase::InitiateKad));

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

        wboot_phase(Some(BootPhase::CollecringKeys(node_data.len())));
        let mut query_pool = node_data
            .iter()
            .map(|nd| nd.sign.0.to_vec().into())
            .map(|id| swarm.behaviour_mut().kad.get_record(id))
            .zip(node_data.iter())
            .collect::<Vec<_>>();
        let mut nodes = HashMap::new();
        while node_data.len() - nodes.len() > 0 {
            let (result, id, last) = match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Kad(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: QueryResult::GetRecord(result),
                        step: ProgressStep { last, .. },
                        ..
                    },
                )) => (result, id, last),
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => continue,
                _ => continue,
            };

            let Some(index) = query_pool.iter().position(|q| q.0 == id) else {
                continue;
            };
            let (.., nd) = query_pool.swap_remove(index);

            let Ok(GetRecordOk::FoundRecord(PeerRecord { record, .. })) = result else {
                if last {
                    log::error!("failed to fetch node record {:#?}", result);
                }
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
            wboot_phase(Some(BootPhase::CollecringKeys(
                node_data.len() - nodes.len(),
            )));
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
            .dispatch_direct::<SearchPeers>(&mut init_stream, &Reminder(&profile_hash.sign.0))
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

        let (mut account_nonce, Reminder(vault)) = match request_dispatch
            .dispatch_direct::<FetchVault>(&mut profile_stream, &profile_hash.sign)
            .await
            .unwrap()
        {
            Ok((n, v)) => (n + 1, v),
            Err(_) => Default::default(),
        };

        let vault = if vault.is_empty() && account_nonce == 0 {
            let proof = Proof::for_profile(&keys.sign, &mut account_nonce);
            request_dispatch
                .dispatch_direct::<CreateAccount>(
                    &mut profile_stream,
                    &(proof, keys.enc.public_key().into_bytes()),
                )
                .await
                .unwrap()
                .unwrap();

            Default::default()
        } else {
            let mut vault = vault.to_vec();
            decrypt(&mut vault, keys.vault)
                .and_then(|v| Vault::decode(&mut &*v))
                .unwrap_or_default()
        };

        let _ = vault.theme.apply();

        wboot_phase(Some(BootPhase::ChatSearch));

        let mut profile_sub = Subscription {
            id: profile_stream_id,
            peer_id: profile_stream_peer,
            topics: [profile_hash.sign.0.to_vec()].into(),
            stream: profile_stream,
        };

        // we clone a lot, but fuck it
        let mut topology = HashMap::<PeerId, HashSet<ChatName>>::new();
        let mut discovered = 0;
        for (peers, chat) in request_dispatch
            .dispatch_direct_batch::<SearchPeers>(
                &mut profile_sub.stream,
                vault
                    .chats
                    .keys()
                    .copied()
                    .map(|c| c.to_bytes())
                    .collect::<Vec<_>>()
                    .iter()
                    .map(|c| Reminder(c.as_slice())),
            )
            .await
            .unwrap()
        {
            if peers.contains(&profile_sub.peer_id) {
                profile_sub.topics.insert(chat.0.to_owned());
                continue;
            }

            let chat = ChatName::decode(&mut &*chat.0).unwrap();
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
        subscriptions.push(profile_sub);
        while !awaiting.is_empty() {
            let (stream, subs, peer_id, id) =
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
                swarm,
                nodes,
                peer_search,
                subscriptions,
                pending_requests: vec![],
                active_subs: vec![],
                pending_topic_search: vec![],
                requests: commands,
            },
            vault,
            request_dispatch,
            account_nonce,
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
                (_id, response) = self.subscriptions.select_next_some() => self.handle_subscription_response(response).await,
            }
        }
    }

    fn handle_topic_search(&mut self, command: RequestInit) {
        let mut buf = Vec::new();
        let sub = self.subscriptions.iter_mut().next().unwrap();
        let search_key = match &command {
            RequestInit::Request(req) => req.topic.as_ref().unwrap(),
            RequestInit::Subscription(sub) => &sub.topic,
            _ => unreachable!(),
        };
        let id = RequestDispatch::<Server>::build_packet::<SearchPeers>(
            &Reminder(search_key.as_slice()),
            &mut buf,
        );
        sub.stream.write(&mut buf);
        log::debug!("searching for {:?}", search_key);
        self.pending_topic_search.push((Ok(id), command));
    }

    fn handle_request(&mut self, mut req: RawRequest) {
        let Some(sub) = self
            .subscriptions
            .iter_mut()
            .find(|s| req.topic.as_ref().map_or(true, |t| s.topics.contains(t)))
        else {
            self.handle_topic_search(RequestInit::Request(req));
            return;
        };

        assert!(!req.payload.is_empty());

        sub.stream.write(&mut req.payload);
        self.pending_requests.push((req.request_id, req.channel));
        log::warn!("request sent, {:?}", req.request_id);
    }

    fn handle_subscription_request(&mut self, mut sub: SubscriptionInit) {
        let Some(subs) = self
            .subscriptions
            .iter_mut()
            .find(|s| s.topics.contains(&sub.topic))
        else {
            self.handle_topic_search(RequestInit::Subscription(sub));
            return;
        };

        assert!(!sub.payload.is_empty());

        log::debug!("subscription request sent");
        subs.stream.write(&mut sub.payload);
        self.active_subs.push((sub.request_id, sub.channel));
    }

    fn handle_command(&mut self, command: RequestInit) {
        match command {
            RequestInit::Request(req) => self.handle_request(req),
            RequestInit::Subscription(sub) => self.handle_subscription_request(sub),
            RequestInit::CloseSubscription(id) => {
                let index = self
                    .active_subs
                    .iter()
                    .position(|(rid, ..)| *rid == id)
                    .expect("subscription not found");
                self.active_subs.swap_remove(index);
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                stream,
                id,
                peer_id,
            ))) => {
                if let Some(index) = self
                    .pending_topic_search
                    .iter()
                    .position(|(pid, ..)| *pid == Err(id))
                {
                    let (_, req) = self.pending_topic_search.swap_remove(index);
                    let topic = match &req {
                        RequestInit::Request(req) => req.topic.as_ref().unwrap(),
                        RequestInit::Subscription(sub) => &sub.topic,
                        _ => unreachable!(),
                    };
                    self.subscriptions.push(Subscription {
                        id,
                        peer_id,
                        topics: [topic.to_owned()].into(),
                        stream,
                    });

                    self.handle_command(req);
                    return;
                }
            }
            e if Self::try_handle_common_event(&e, &mut self.swarm, &mut self.peer_search) => {}
            e => log::debug!("{:?}", e),
        }
    }

    async fn handle_subscription_response(&mut self, request: io::Result<Vec<u8>>) {
        let Ok(msg) = request.inspect_err(|e| log::error!("chat subscription error: {e}")) else {
            return;
        };

        let Some(DispatchResponse {
            request_id,
            response: Reminder(content),
        }) = DispatchResponse::decode(&mut &*msg)
        else {
            log::error!("invalid chat subscription response");
            return;
        };

        if let Some(index) = self
            .pending_requests
            .iter()
            .position(|(rid, ..)| *rid == request_id)
        {
            let (_, channel) = self.pending_requests.swap_remove(index);
            _ = channel.send(content.to_owned());
            return;
        }

        if let Some((_, channel)) = self
            .active_subs
            .iter_mut()
            .find(|(id, ..)| *id == request_id)
        {
            if channel.send(content.to_owned()).await.is_err() {
                let index = self
                    .active_subs
                    .iter()
                    .position(|(id, ..)| *id == request_id)
                    .expect("subscription not found");
                self.active_subs.swap_remove(index);
            };
            return;
        }

        if let Some(index) = self
            .pending_topic_search
            .iter()
            .position(|(id, ..)| *id == Ok(request_id))
        {
            log::debug!("chat subscription response found");
            let (_, req) = self.pending_topic_search.swap_remove(index);
            let Ok(resp) = RequestDispatch::<Server>::parse_response::<SearchPeers>(content) else {
                log::error!("invalid chat subscription response");
                return;
            };

            if let Some(sub) = self
                .subscriptions
                .iter_mut()
                .find(|s| resp.iter().any(|p| s.peer_id == *p))
            {
                log::debug!("chat subscription response found");
                let topic = match &req {
                    RequestInit::Request(req) => req.topic.as_ref().unwrap(),
                    RequestInit::Subscription(sub) => &sub.topic,
                    _ => unreachable!(),
                };
                sub.topics.insert(topic.to_owned());

                let (_, req) = self.pending_topic_search.swap_remove(index);
                self.handle_command(req);
                return;
            }

            log::debug!("chat subscription response not found");
            let Some(pick) = resp.into_iter().choose(&mut rand::thread_rng()) else {
                log::error!("no peers found");
                return;
            };

            let path = pick_route(&self.nodes, pick);
            let pid = self.swarm.behaviour_mut().onion.open_path(path).unwrap();
            self.pending_topic_search.push((Err(pid), req));
            return;
        }

        log::error!(
            "request does not exits enev though we recieived it {:?}",
            request_id
        );
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
