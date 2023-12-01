use {
    crate::{BootPhase, UserKeys},
    anyhow::Context,
    chat_logic::{
        ChatName, CreateAccount, DispatchResponse, FetchVault, Identity, Nonce, Proof, RawChatName,
        RawRequest, RawResponse, RequestDispatch, RequestId, RequestInit, RequestStream,
        SearchPeers, Server, SubscriptionInit, SubscriptionMessage,
    },
    component_utils::{
        find_and_remove,
        futures::{self},
        kad::KadPeerSearch,
        Codec, LinearMap, Reminder,
    },
    crypto::{
        decrypt,
        enc::{self, ChoosenCiphertext, Ciphertext},
        sign, FixedAesPayload, Serialized, TransmutationCircle,
    },
    leptos::signal_prelude::*,
    libp2p::{
        core::{upgrade::Version, ConnectedPoint},
        futures::{SinkExt, StreamExt},
        kad::{store::MemoryStore, GetRecordOk, PeerRecord, ProgressStep, QueryResult},
        swarm::{NetworkBehaviour, SwarmEvent},
        PeerId, Swarm, *,
    },
    onion::{EncryptedStream, PathId, SharedSecret},
    primitives::{
        contracts::{NodeIdentity, StoredUserIdentity},
        RawUserName, UserName,
    },
    rand::seq::IteratorRandom,
    std::{
        collections::{HashMap, HashSet, VecDeque},
        io, mem,
        task::Poll,
        time::Duration,
    },
    web_sys::wasm_bindgen::JsValue,
};

pub type MessageContent = String;

pub struct JoinRequestPayload {
    pub name: RawUserName,
    pub chat: RawChatName,
    pub identity: Identity,
}

crypto::impl_transmute!(JoinRequestPayload,);

component_utils::protocol! { 'a:
    #[derive(Default)]
    struct Vault {
        chats: LinearMap<ChatName, ChatMeta>,
        hardened_chats: LinearMap<ChatName, HardenedChatMeta>,
        theme: Theme,
    }

    struct ChatMeta {
        secret: crypto::SharedSecret,
        action_no: Nonce,
    }

    #[derive(Default)]
    struct HardenedChatMeta {
        members: LinearMap<UserName, MemberMeta>,
        messages: VecDeque<SavedHardenedChatMessage>,
    }

    #[derive(Clone, Copy)]
    struct MemberMeta {
        secret: crypto::SharedSecret,
        identity: crypto::Hash<sign::PublicKey>,
    }

    struct RawChatMessage<'a> {
        sender: UserName,
        content: &'a str,
    }

    enum Mail<'a> {
        ChatInvite: ChatInvite,
        HardenedJoinRequest: HardenedJoinRequest,
        HardenedChatInvite: HardenedChatInvite<'a>,
        HardenedChatMessage: HardenedChatMessage<'a>,
    }

    struct ChatInvite {
        chat: ChatName,
        cp: Serialized<ChoosenCiphertext>,
    }

    struct HardenedJoinRequest {
        cp: Serialized<Ciphertext>,
        payload: [u8; std::mem::size_of::<FixedAesPayload<{ std::mem::size_of::<JoinRequestPayload>() }>>()],
    }

    struct HardenedChatMessage<'a> {
        nonce: Nonce,
        chat: crypto::AnyHash,
        content: Reminder<'a>,
    }

    #[derive(Clone)]
    struct SavedHardenedChatMessage {
        sender: UserName,
        content: String,
    }

    struct HardenedChatInvite<'a> {
        cp: Serialized<Ciphertext>,
        payload: Reminder<'a>,
    }

    struct HardenedChatInvitePayload {
        chat: ChatName,
        inviter: UserName,
        inviter_id: Identity,
        members: Vec<UserName>,
    }
}

impl ChatMeta {
    pub fn new() -> Self {
        Self {
            secret: crypto::new_secret(),
            action_no: 1,
        }
    }

    pub fn from_secret(secret: SharedSecret) -> Self {
        Self {
            secret,
            action_no: 1,
        }
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
    pending_requests: LinearMap<RequestId, libp2p::futures::channel::oneshot::Sender<RawResponse>>,
    pending_topic_search: LinearMap<Result<RequestId, PathId>, RequestInit>,
    active_subs: LinearMap<RequestId, libp2p::futures::channel::mpsc::Sender<SubscriptionMessage>>,
    nodes: HashMap<PeerId, enc::PublicKey>,
    requests: RequestStream,
}

impl Node {
    pub async fn new(
        keys: UserKeys,
        wboot_phase: WriteSignal<Option<BootPhase>>,
    ) -> anyhow::Result<(Self, Vault, RequestDispatch<Server>, Nonce)> {
        macro_rules! set_state { ($($t:tt)*) => {wboot_phase(Some(BootPhase::$($t)*))}; }

        set_state!(FetchNodesAndProfile);

        let mut peer_search = KadPeerSearch::default();
        let (mut request_dispatch, commands) = RequestDispatch::new();
        let chain_api = crate::chain::node(keys.name).await?;
        let node_request = chain_api.list(crate::chain::node_contract());
        let profile_request =
            chain_api.get_profile_by_name(crate::chain::user_contract(), keys.name);
        let (node_data, profile_hash) = futures::try_join!(node_request, profile_request)?;
        let profile_hash = profile_hash.context("profile not found")?;
        let profile_hash = StoredUserIdentity::from_bytes(profile_hash);
        let profile = keys.to_identity();

        anyhow::ensure!(
            profile_hash.verify(&profile),
            "profile hash does not match our account"
        );

        set_state!(InitiateConnection);

        let keypair = identity::Keypair::generate_ed25519();
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
        let transport = websocket_websys::Transport::new(100)
            .upgrade(Version::V1)
            .authenticate(noise::Config::new(&keypair).unwrap())
            .multiplex(yamux::Config::default())
            .boxed();

        let mut swarm = swarm::Swarm::new(
            transport,
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_wasm_executor().with_idle_connection_timeout(Duration::MAX), // TODO: please, dont
        );
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));

        swarm.dial(crate::chain::boot_node()).unwrap();
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

        set_state!(Bootstrapping);

        let qid = swarm
            .behaviour_mut()
            .kad
            .bootstrap()
            .expect("to have enough peers");
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

        set_state!(CollecringKeys(node_data.len()));

        let mut query_pool = node_data
            .iter()
            .map(|nd| nd.sign.0.to_vec().into())
            .map(|id| swarm.behaviour_mut().kad.get_record(id))
            .zip(node_data.iter())
            .collect::<Vec<_>>();
        let mut nodes = HashMap::new();
        while !query_pool.is_empty() {
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

            log::debug!("got node record {:#?}", result);

            let Some((.., nd)) = find_and_remove(&mut query_pool, |&(qid, ..)| qid == id) else {
                continue;
            };

            let Ok(GetRecordOk::FoundRecord(PeerRecord { record, .. })) = result else {
                if last {
                    log::error!("failed to fetch node record {:#?}", result);
                } else {
                    query_pool.push((id, nd));
                }
                continue;
            };

            let Some(identity) = NodeIdentity::try_from_slice(&record.value) else {
                continue;
            };

            let Ok(pk) = libp2p::identity::ed25519::PublicKey::try_from_bytes(&nd.id)
                .map(libp2p::identity::PublicKey::from)
                .inspect_err(|e| log::error!("failed to construct node publick key: {e}"))
            else {
                continue;
            };

            nodes.insert(pk.to_peer_id(), identity.enc);
            set_state!(CollecringKeys(query_pool.len()));
        }

        anyhow::ensure!(
            nodes.len() >= crate::chain::min_nodes(),
            "not enough nodes in network, needed {}, got {}",
            crate::chain::min_nodes(),
            nodes.len(),
        );

        set_state!(InitialRoute);

        let route = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .choose_multiple(&mut rand::thread_rng(), 3)
            .try_into()
            .unwrap();

        let pid = swarm.behaviour_mut().onion.open_path(route)?;
        let (mut init_stream, init_stream_id, init_stream_per) = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                    peer,
                ))) if id == pid => break (stream, id, peer),
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                e => log::debug!("{:?}", e),
            }
        };

        set_state!(ProfileSearch);

        let members = request_dispatch
            .dispatch_direct::<SearchPeers>(&mut init_stream, &Reminder(&profile_hash.sign.0))
            .await
            .context("searching profile replicators")?;

        set_state!(ProfileLoad);

        let (mut profile_stream, profile_stream_id, profile_stream_peer) =
            if members.contains(&init_stream_per) {
                (init_stream, init_stream_id, init_stream_per)
            } else {
                let pick = members.into_iter().choose(&mut rand::thread_rng()).unwrap();
                let route = pick_route(&nodes, pick);
                let pid = swarm.behaviour_mut().onion.open_path(route)?;
                loop {
                    match swarm.select_next_some().await {
                        SwarmEvent::Behaviour(BehaviourEvent::Onion(
                            onion::Event::OutboundStream(stream, id, _),
                        )) if id == pid => break (stream, id, pick),
                        e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                        e => log::debug!("{:?}", e),
                    }
                }
            };
        let (mut account_nonce, Reminder(vault)) = match request_dispatch
            .dispatch_direct::<FetchVault>(&mut profile_stream, &profile_hash.sign)
            .await
            .context("fetching vault")?
        {
            Ok((n, v)) => (n + 1, v),
            Err(_) => Default::default(),
        };
        let vault = if vault.is_empty() && account_nonce == 0 {
            let proof = Proof::for_profile(&keys.sign, &mut account_nonce);
            request_dispatch
                .dispatch_direct::<CreateAccount>(
                    &mut profile_stream,
                    &(proof, keys.enc.public_key().into_bytes(), Reminder(&[])),
                )
                .await
                .context("creating account")?
                .context("creating account")?;

            Default::default()
        } else {
            let mut vault = vault.to_vec();
            decrypt(&mut vault, keys.vault)
                .and_then(|v| Vault::decode(&mut &*v))
                .unwrap_or_default()
        };
        let _ = vault.theme.apply();

        set_state!(ChatSearch);

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
            .await?
        {
            if peers.contains(&profile_sub.peer_id) {
                profile_sub.topics.insert(chat.0.to_owned());
                continue;
            }

            let chat =
                ChatName::decode(&mut &*chat.0).expect("for just encoded chat to be decodable");
            for peer in peers {
                topology.entry(peer).or_default().insert(chat);
            }
            discovered += 1;
        }

        set_state!(ChatLoad);

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
                let pid = swarm
                    .behaviour_mut()
                    .onion
                    .open_path(route)
                    .expect("client to never fail");
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
                            if let Some((.., peer_id, subs)) =
                                find_and_remove(&mut awaiting, |&(i, ..)| i == id)
                            {
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
                topics: subs.into_iter().map(|c| c.to_bytes()).collect(),
                stream,
            });
        }

        set_state!(ChatRun);

        Ok((
            Self {
                swarm,
                nodes,
                peer_search,
                subscriptions,
                pending_requests: Default::default(),
                active_subs: Default::default(),
                pending_topic_search: Default::default(),
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

    pub async fn run(mut self) -> anyhow::Result<()> {
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
        let search_key = command.topic();
        let id =
            RequestDispatch::<Server>::build_packet::<SearchPeers>(&Reminder(search_key), &mut buf);
        sub.stream.write(&mut buf);
        log::debug!("searching for {:?}", search_key);
        self.pending_topic_search.insert(Ok(id), command);
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
        self.pending_requests.insert(req.id, req.channel);
        log::debug!("request sent, {:?}", req.id);
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

        subs.stream.write(&mut sub.payload);
        self.active_subs.insert(sub.request_id, sub.channel);
        log::debug!("subscription request send, {:?}", sub.request_id);
    }

    fn handle_command(&mut self, command: RequestInit) {
        match command {
            RequestInit::Request(req) => self.handle_request(req),
            RequestInit::Subscription(sub) => self.handle_subscription_request(sub),
            RequestInit::CloseSubscription(id) => _ = self.active_subs.remove(&id).unwrap(),
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                stream,
                id,
                peer_id,
            ))) => {
                if let Some(req) = self.pending_topic_search.remove(&Err(id)) {
                    self.subscriptions.push(Subscription {
                        id,
                        peer_id,
                        topics: [req.topic().to_owned()].into(),
                        stream,
                    });

                    self.handle_command(req);
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

        if let Some(channel) = self.pending_requests.remove(&request_id) {
            _ = channel.send(content.to_owned());
            return;
        }

        if let Some(channel) = self.active_subs.get_mut(&request_id) {
            if channel.send(content.to_owned()).await.is_err() {
                self.active_subs.remove(&request_id).unwrap();
            }
            return;
        }

        if let Some(req) = self.pending_topic_search.remove(&Ok(request_id)) {
            log::debug!("received pending topic search query");
            let Ok(resp) = RequestDispatch::<Server>::parse_response::<SearchPeers>(content) else {
                log::error!("search query response is invalid");
                return;
            };

            if let Some(sub) = self
                .subscriptions
                .iter_mut()
                .find(|s| resp.iter().any(|p| s.peer_id == *p))
            {
                log::debug!("shortcut topic found");
                sub.topics.insert(req.topic().to_owned());
                self.handle_command(req);
                return;
            }

            let Some(pick) = resp.into_iter().choose(&mut rand::thread_rng()) else {
                log::error!("search response does not contain any peers");
                return;
            };

            let path = pick_route(&self.nodes, pick);
            let pid = self.swarm.behaviour_mut().onion.open_path(path).unwrap();
            self.pending_topic_search.insert(Err(pid), req);
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
