use chain_api::NodeData;
use component_utils::futures::stream::Fuse;
use component_utils::futures::{self, FutureExt};
use component_utils::kad::KadPeerSearch;
use component_utils::Codec;
use crypto::{decrypt, SharedSecret};
use leptos::signal_prelude::*;
use libp2p::core::upgrade::Version;
use libp2p::core::ConnectedPoint;
use libp2p::futures::StreamExt;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::{ConnectionHandler, NetworkBehaviour, SwarmEvent};
use libp2p::*;
use libp2p::{PeerId, Swarm};
use onion::{EncryptedStream, PathId};
use protocols::chat::*;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::task::Poll;
use std::time::Duration;
use std::{future, io, mem, pin::Pin, usize};
use std::{iter, u8};

pub type MessageContent = std::rc::Rc<str>;

component_utils::protocol! { 'a:
    #[derive(Default)]
    struct Vault {
        chats: Vec<ChatMeta>,
        theme: Theme,
        action_no: ActionNo,
    }

    struct ChatMeta {
        name: ChatName,
        secret: crypto::SharedSecret,
        salt: [u8; protocols::chat::SALT_SIZE],
        message_no: ActionNo,
        permission: Permission,
    }

    struct Theme {
        primary: u32,
        secondary: u32,
        hihlight: u32,
        font: u32,
        error: u32,
    }

    struct RawChatMessage<'a> {
        user: UserName,
        content: &'a str,
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            primary: 0x000000ff,
            secondary: 0x444444ff,
            hihlight: 0xffffffff,
            font: 0xffffffff,
            error: 0xff0000ff,
        }
    }
}

#[derive(Clone)]
pub enum Event {
    NewMessage {
        chat: ChatName,
        name: UserName,
        content: MessageContent,
    },
    FetchedMessages {
        chat: ChatName,
        messages: Vec<u8>,
    },
    None,
}

#[derive(Clone)]
pub enum Command {
    SendMessage {
        chat: ChatName,
        content: String,
    },
    #[allow(dead_code)]
    FetchMessages(ChatName),
    None,
}

enum SearchRouteState {
    Established(EncryptedStream),
    Reestablishing(PathId),
}

impl SearchRouteState {
    pub async fn poll(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::Established(s) => match s.select_next_some().await {
                Ok(o) => Some(o),
                Err(err) => {
                    log::error!("search route error: {}", err);
                    None
                }
            },
            Self::Reestablishing(..) => future::pending().await,
        }
    }
}

pub struct Node {
    events: WriteSignal<Event>,
    commands: Fuse<Pin<Box<dyn futures::Stream<Item = Command>>>>,
    username: UserName,
    keys: UserKeys,
    swarm: Swarm<Behaviour>,
    peer_search: KadPeerSearch,
    profile_path: SearchRouteState,
    subscriptions: futures::stream::SelectAll<Subscription>,
    nodes: HashMap<PeerId, NodeData>,
    buffer: Vec<u8>,
    buffer2: Vec<u8>,
    vault: Vault,
}

impl Node {
    pub async fn new(
        keys: UserKeys,
        chain_bootstrap_node: &str,
        events: WriteSignal<Event>,
        commands: ReadSignal<Command>,
    ) -> Result<Self, BootError> {
        let node_request = chain_api::nodes(chain_bootstrap_node);
        let profile_request = chain_api::user_by_sign(chain_bootstrap_node, keys.identity().sign);
        let (node_data, profile) = futures::join!(node_request, profile_request);
        let (node_data, profile) = (
            node_data.map_err(BootError::FetchNodes)?,
            profile.map_err(BootError::FetchProfile)?,
        );

        let nodes = node_data
            .into_iter()
            .map(node_data_to_path_seg)
            .collect::<HashMap<_, _>>();

        if nodes.len() < MIN_NODES {
            return Err(BootError::NotEnoughNodes(nodes.len()));
        }

        let route @ [(enter_node, ..), ..]: [_; 3] = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .take(3)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let keypair = identity::Keypair::ed25519_from_bytes(keys.sign.ed).unwrap();
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
                    kad::Config::default()
                        .set_replication_factor(protocols::chat::REPLICATION_FACTOR),
                ),
            ),
        };

        let mut swarm = swarm::Swarm::new(
            transport,
            behaviour,
            peer_id,
            libp2p::swarm::Config::with_wasm_executor().with_idle_connection_timeout(Duration::MAX), // TODO: please, dont
        );

        use libp2p::core::multiaddr::Protocol;
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));
        swarm
            .dial(
                Multiaddr::empty()
                    .with(Protocol::Ip4(enter_node.ip.into()))
                    .with(Protocol::Tcp(enter_node.port + 100)) // uh TODO
                    .with(Protocol::Ws("/".into())),
            )
            .unwrap();

        loop {
            if let SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint: ConnectedPoint::Dialer { address, .. },
                ..
            } = swarm.select_next_some().await
            {
                swarm.behaviour_mut().kad.add_address(&peer_id, address);
                break;
            }
        }

        let mut peer_search = KadPeerSearch::default();

        let pid = swarm
            .behaviour_mut()
            .onion
            .open_path(route.map(|(u, p)| (u.enc.into(), p)))
            .unwrap();
        let mut init_stream = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest {
                    to,
                })) => {
                    log::error!("{to}");
                    component_utils::handle_conn_request(to, &mut swarm, &mut peer_search);
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                    if component_utils::try_handle_conn_response(
                        &e,
                        &mut swarm,
                        &mut peer_search,
                    ) =>
                {
                    log::error!("{e:?}");
                }
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                ))) if id == pid => break stream,
                e => log::debug!("{:?}", e),
            }
        };

        let mut buffer = vec![];

        send_request(
            InitRequest::Search(profile.sign),
            &mut init_stream,
            &mut buffer,
        );

        log::debug!("foo");
        let resp = init_stream.next().await.unwrap().unwrap();
        let Some(InitSearchResult { members, key }) = <_>::decode(&mut resp.as_slice()) else {
            todo!("error handling");
        };

        if key != profile.sign {
            todo!("error handling");
        }

        let Some(pick) = members
            .into_iter()
            .collect::<HashSet<_>>() // kind of a suffle
            .into_iter()
            .find_map(|m| Some((*nodes.get(&m)?, m)))
        else {
            todo!("error handling")
        };

        let hash = RandomState::new().hash_one(key) as usize;

        let route: [_; 3] = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .cycle()
            .skip(hash % nodes.len())
            .take(2)
            .chain(iter::once(pick))
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let pid = swarm
            .behaviour_mut()
            .onion
            .open_path(route.map(|(ud, p)| (ud.enc.into(), p)))
            .unwrap();

        let mut profile_stream = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest {
                    to,
                })) => {
                    component_utils::handle_conn_request(to, &mut swarm, &mut peer_search);
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                    if component_utils::try_handle_conn_response(
                        &e,
                        &mut swarm,
                        &mut peer_search,
                    ) => {}
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                ))) if id == pid => break stream,
                _ => {}
            }
        };

        send_request(
            InitRequest::ReadData(profile.sign),
            &mut profile_stream,
            &mut buffer,
        );

        let mut resp = profile_stream.next().await.unwrap().unwrap();
        let mut vault = if resp.is_empty() {
            Vault::default()
        } else {
            let foo = decrypt(&mut resp, keys.vault).unwrap();
            Vault::decode(&mut &*foo).unwrap()
        };

        send_request(
            ProfileRequest::Subscribe(ActionProof::for_profile(&mut vault.action_no, &keys.sign)),
            &mut profile_stream,
            &mut buffer,
        );

        for chat in vault.chats.iter() {
            send_request(
                ProfileRequest::Search(chat.name),
                &mut profile_stream,
                &mut buffer,
            );
        }

        let mut awaiting = vec![];

        for chat in vault.chats.iter() {
            send_request(
                ProfileRequest::Search(chat.name),
                &mut profile_stream,
                &mut buffer,
            );
        }

        let mut topology = HashMap::<PeerId, HashSet<ChatName>>::new();
        let mut discovered = 0;
        for _ in 0..vault.chats.len() {
            let resp = match profile_stream.next().await.unwrap() {
                Ok(resp) => resp,
                Err(e) => return Err(BootError::ChatSearch(e)),
            };

            let Some(ProfileResponse::Search(ChatSearchResult { members, key })) =
                <_>::decode(&mut &*resp)
            else {
                continue;
            };

            if vault.chats.iter().all(|c| c.name != key) {
                continue;
            }

            discovered += 1;

            for member in members {
                topology.entry(member).or_default().insert(key);
            }
        }

        let mut topology = topology.into_iter().collect::<Vec<_>>();
        topology.sort_by_key(|(_, v)| usize::MAX - v.len());

        let mut to_connect = vec![];
        let mut seen = HashSet::new();
        while seen.len() < discovered {
            let (peer, chats) = topology.pop().unwrap();
            if !chats.iter().fold(false, |acc, &ch| acc | seen.insert(ch)) {
                continue;
            }
            to_connect.push((peer, chats));
        }

        to_connect
            .into_iter()
            .map(|(pick, set)| {
                let hash = RandomState::new().hash_one(pick) as usize;

                let nd = *nodes.get(&pick).unwrap();

                let route: [_; 3] = nodes
                    .iter()
                    .map(|(a, b)| (*b, *a))
                    .cycle()
                    .skip(hash % nodes.len())
                    .take(2)
                    .chain(iter::once((nd, pick)))
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap();

                (
                    swarm
                        .behaviour_mut()
                        .onion
                        .open_path(route.map(|(ud, p)| (ud.enc.into(), p)))
                        .unwrap(),
                    set,
                )
            })
            .collect_into(&mut awaiting);

        let mut subscriptions = futures::stream::SelectAll::new();
        while !awaiting.is_empty() {
            let (mut stream, subs, id) = loop {
                match swarm.select_next_some().await {
                    SwarmEvent::Behaviour(BehaviourEvent::Onion(
                        onion::Event::ConnectRequest { to },
                    )) => {
                        component_utils::handle_conn_request(to, &mut swarm, &mut peer_search);
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                        if component_utils::try_handle_conn_response(
                            &e,
                            &mut swarm,
                            &mut peer_search,
                        ) => {}
                    SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                        stream,
                        id,
                    ))) => {
                        if let Some(i) = awaiting.iter().position(|&(i, ..)| i == id) {
                            break (stream, awaiting.swap_remove(i).1, id);
                        }
                    }
                    _ => {}
                }
            };

            send_request(
                InitRequest::Subscribe(subs.iter().copied().collect()),
                &mut stream,
                &mut buffer,
            );

            subscriptions.push(Subscription {
                id,
                subs: subs
                    .into_iter()
                    .filter_map(|c| {
                        if let Some(meta) = vault.chats.iter().find(|m| m.name == c) {
                            Some((c, meta.secret))
                        } else {
                            log::warn!("chat {c} not found in vault");
                            None
                        }
                    })
                    .collect(),
                stream,
                cursor: protocols::chat::NO_CURSOR,
            });
        }

        Ok(Self {
            keys,
            events,
            commands: commands.to_stream().fuse(),
            swarm,
            peer_search,
            profile_path: SearchRouteState::Established(profile_stream),
            subscriptions,
            nodes,
            buffer,
            buffer2: vec![],
            username: profile.name,
            vault,
        })
    }

    pub async fn run(mut self) {
        loop {
            futures::select! {
                event = self.swarm.select_next_some() => self.handle_swarm_event(event),
                command = self.commands.select_next_some() => self.handle_command(command),
                search_packet = self.profile_path.poll().fuse() => self.handle_profile_response(search_packet),
                (id, response) = self.subscriptions.select_next_some() => self.handle_subscription_response(id, response),
            }
        }
    }

    fn handle_profile_response(&mut self, packet: Option<Vec<u8>>) {
        let Some(packet) = packet else {
            let route: [_; 3] = self
                .nodes
                .iter()
                .map(|(a, b)| (*b, *a))
                .take(3)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap();

            self.profile_path = SearchRouteState::Reestablishing(
                self.swarm
                    .behaviour_mut()
                    .onion
                    .open_path(route.map(|(u, p)| (u.enc.into(), p)))
                    .unwrap(),
            );
            return;
        };

        let Some(resp) = ProfileResponse::decode(&mut packet.as_slice()) else {
            log::error!("profile response is malformed");
            return;
        };

        match resp {
            ProfileResponse::Mail(_) => {}
            ProfileResponse::DataWritten => log::debug!("vault written"),
            ProfileResponse::DataWriteFailed(e) => log::error!("vault write failed: {e}"),
            ProfileResponse::Search(_) => todo!(),
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest { to })) => {
                component_utils::handle_conn_request(to, &mut self.swarm, &mut self.peer_search);
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(
                    &e,
                    &mut self.swarm,
                    &mut self.peer_search,
                ) => {}
            e => log::debug!("{:?}", e),
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::SendMessage { chat, content } => {
                let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| s.subs.contains_key(&chat))
                else {
                    log::error!("chat not found when sending message");
                    return;
                };

                let Some(meta) = self.vault.chats.iter_mut().find(|s| s.name == chat) else {
                    log::error!("chat meta not found when sending message");
                    return;
                };

                self.buffer.clear();
                RawChatMessage {
                    user: self.username,
                    content: &content,
                }
                .encode(&mut self.buffer);
                crypto::encrypt(&mut self.buffer, *sub.subs.get(&chat).unwrap());

                send_request(
                    ChatRequest::Send(Message {
                        chat,
                        content: &self.buffer,
                        proof: ActionProof::for_chat(&mut meta.message_no, &self.keys.sign, chat),
                    }),
                    &mut sub.stream,
                    &mut self.buffer2,
                );
            }
            Command::FetchMessages(chat) => {
                let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| s.subs.contains_key(&chat))
                else {
                    log::error!("chat not found when sending message");
                    return;
                };

                send_request(
                    ChatRequest::Fetch(FetchMessages {
                        chat,
                        cursor: sub.cursor,
                    }),
                    &mut sub.stream,
                    &mut self.buffer,
                );
            }
            Command::None => {}
        }
    }

    fn handle_subscription_response(&mut self, id: PathId, request: io::Result<Vec<u8>>) {
        let msg = match request {
            Ok(m) => m,
            Err(e) => {
                log::error!("chat subscription error: {e}");
                return;
            }
        };

        let Some(pckt) = ChatResponse::decode(&mut msg.as_slice()) else {
            log::error!("chat subscription packet is malformed");
            return;
        };

        let Some(sub) = self.subscriptions.iter_mut().find(|s| s.id == id) else {
            log::error!("subscription not found");
            return;
        };

        match pckt {
            ChatResponse::New(msg) => {
                let Some(&secret) = sub.subs.get(&msg.chat) else {
                    log::warn!("message chat does not match subscription");
                    return;
                };

                if msg.proof.is_chat_valid(msg.chat) {
                    log::warn!("message chat is invalid");
                    return;
                }

                let mut content = msg.content.to_vec();
                let Some(decrypted) = crypto::decrypt(&mut content, secret) else {
                    log::warn!("message content is malformed, cannot decrypt");
                    return;
                };

                let Some(RawChatMessage { user, content }) = <_>::decode(&mut &*decrypted) else {
                    log::warn!("message is decriptable but still malformed");
                    return;
                };

                (self.events)(Event::NewMessage {
                    chat: msg.chat,
                    name: user,
                    content: content.into(),
                });
            }
            ChatResponse::Fetched(fmsg @ FetchedMessages { chat, cursor, .. }) => {
                let messages = fmsg.messages.to_vec();
                sub.cursor = cursor;
                (self.events)(Event::FetchedMessages { chat, messages });
            }
            ChatResponse::Failed(e) => log::error!("failed to fetch messages: {}", e),
            ChatResponse::NotFound => log::error!("chat not found"),
        }
    }

    pub fn username(&self) -> UserName {
        self.username
    }

    pub fn chats(&self) -> impl Iterator<Item = ChatName> + '_ {
        self.vault.chats.iter().map(|c| c.name)
    }
}

pub fn send_request<'a, T: Codec<'a>>(resp: T, stream: &mut EncryptedStream, buffer: &mut Vec<u8>) {
    buffer.clear();
    resp.encode(buffer);
    stream.write(buffer);
}

#[allow(deprecated)]
type SE = libp2p::swarm::SwarmEvent<
    <Behaviour as NetworkBehaviour>::ToSwarm,
    <<Behaviour as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::Error,
>;

const MIN_NODES: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error("not enough nodes: {0} < {MIN_NODES}")]
    NotEnoughNodes(usize),
    #[error(transparent)]
    Encapsulation(#[from] crypto::enc::EncapsulationError),
    #[error("failed to fetch nodes: {0}")]
    FetchNodes(chain_api::NodesError),
    #[error("failed to fetch profile: {0}")]
    FetchProfile(chain_api::GetUserError),
    #[error("failed to search for chat (stream broken): {0}")]
    ChatSearch(io::Error),
}

fn node_data_to_path_seg(data: NodeData) -> (PeerId, NodeData) {
    let peer_id = component_utils::libp2p_identity::PublicKey::from(
        component_utils::libp2p_identity::ed25519::PublicKey::try_from_bytes(
            &crypto::sign::PublicKey::from(data.sign).ed,
        )
        .unwrap(),
    )
    .to_peer_id();
    (peer_id, data)
}

struct Subscription {
    id: PathId,
    subs: HashMap<ChatName, SharedSecret>,
    stream: EncryptedStream,
    cursor: protocols::chat::Cursor,
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
}

component_utils::impl_kad_search!(Behaviour => (onion::Behaviour => onion));
