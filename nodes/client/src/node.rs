use chain_api::{NodeData, UserData};
use component_utils::futures::stream::Fuse;
use component_utils::futures::{self, FutureExt};
use component_utils::kad::KadPeerSearch;
use component_utils::Codec;
use leptos::signal_prelude::*;
use libp2p::core::upgrade::Version;
use libp2p::core::ConnectedPoint;
use libp2p::futures::StreamExt;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::{ConnectionHandler, NetworkBehaviour, SwarmEvent};
use libp2p::*;
use libp2p::{PeerId, Swarm};
use onion::{EncryptedStream, PathId};
use protocols::chat::{
    ActionNo, ChatName, FetchedMessages, MessagePayload, Permission, PrefixedMessage, Request,
    Response, SearchResult, UserKeys, UserName, UserOrChat,
};
use std::collections::{HashMap, HashSet};
use std::task::Poll;
use std::time::Duration;
use std::{future, io, mem, pin::Pin, usize};

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
    Subscribed(ChatName),
    None,
}

#[derive(Clone)]
pub enum Command {
    Subscrbe {
        chat: ChatName,
    },
    SendMessage {
        chat: ChatName,
        name: String,
        content: String,
    },
    #[allow(dead_code)]
    FetchMessages(ChatName),
    None,
}

enum SearchRouteState {
    Established(EncryptedStream),
    Reestablishing(PathId, ()),
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
    nodes: HashMap<PeerId, NodeData>,
    username: UserName,
    keys: UserKeys,
    swarm: Swarm<Behaviour>,
    peer_search: KadPeerSearch,
    search_route: SearchRouteState,
    pending_streams: Vec<(UserOrChat, PathId)>,
    subscriptions: futures::stream::SelectAll<Subscription>,
    temp_buffer: Vec<u8>,
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

        // rely on a fact that hash map has random order of iteration (RandomState)
        let route @ [(enter_node, ..), ..]: [_; 3] = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .take(3)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let profile_nodes: [_; 2] = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .skip(3)
            .take(2)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let keypair = identity::Keypair::ed25519_from_bytes(keys.sign.ed).unwrap();
        let transport = websocket_websys::Transport::new(100)
            .upgrade(Version::V1Lazy)
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
            libp2p::swarm::Config::with_wasm_executor(),
        );

        use libp2p::core::multiaddr::Protocol;
        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));
        swarm
            .dial(
                Multiaddr::empty()
                    .with(Protocol::Ip4(enter_node.ip.into()))
                    .with(Protocol::Tcp(enter_node.port))
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

        async fn open_path(
            swarm: &mut Swarm<Behaviour>,
            peer_search: &mut KadPeerSearch,
            route: [(NodeData, PeerId); 3],
        ) -> Result<EncryptedStream, BootError> {
            let search_route = swarm
                .behaviour_mut()
                .onion
                .open_path(route.map(|(ud, p)| (ud.enc.into(), p)))?;
            loop {
                match swarm.select_next_some().await {
                    SwarmEvent::Behaviour(BehaviourEvent::Onion(
                        onion::Event::ConnectRequest { to },
                    )) => {
                        component_utils::handle_conn_request(to, swarm, peer_search);
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                        if component_utils::try_handle_conn_response(&e, swarm, peer_search) => {}
                    SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                        stream,
                        id,
                    ))) if id == search_route => {
                        break Ok(stream);
                    }
                    _ => {}
                }
            }
        }

        let mut search_stream = open_path(&mut swarm, &mut peer_search, route).await?;
        let mut temp_buffer = vec![];

        Request::SearchFor(UserOrChat::User(profile.sign)).encode(&mut temp_buffer);
        search_stream.write(&mut temp_buffer);

        let resp = search_stream
            .next()
            .await
            .expect("always reture one element")
            .map_err(BootError::ProfileSearch)?;

        let Some(Response::SearchResults(SearchResult {
            members,
            key: UserOrChat::User(key),
        })) = <_>::decode(&mut resp.as_slice())
        else {
            return Err(BootError::ProfileSearch(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected response",
            )));
        };

        if key != profile.sign {
            return Err(BootError::ProfileSearch(io::Error::new(
                io::ErrorKind::InvalidData,
                "profile key mismatch",
            )));
        }

        let Some(pick) = members
            .into_iter()
            .collect::<HashSet<_>>() // kind of a suffle
            .into_iter()
            .find_map(|m| Some((*nodes.get(&m)?, m)))
        else {
            return Err(BootError::ProfileSearch(io::Error::new(
                io::ErrorKind::InvalidData,
                "no node found",
            )));
        };

        let mut profile_path = open_path(
            &mut swarm,
            &mut peer_search,
            [profile_nodes[0], profile_nodes[1], pick],
        )
        .await?;

        temp_buffer.clear();
        Request::ReadData(profile.sign).encode(&mut temp_buffer);
        profile_path.write(&mut temp_buffer);

        let resp = profile_path
            .next()
            .await
            .expect("always reture one element")
            .map_err(BootError::VauldFetch)?;

        let Some(Response::DataRed(data)) = <_>::decode(&mut resp.as_slice()) else {
            return Err(BootError::VauldFetch(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected response",
            )));
        };

        let mut data = data.to_vec();
        let Some(data) = crypto::decrypt(&mut data, keys.vault) else {
            return Err(BootError::VauldFetch(io::Error::new(
                io::ErrorKind::InvalidData,
                "failed to decode vault",
            )));
        };

        let Some(vault) = Vault::decode(&mut &*data) else {
            return Err(BootError::VauldFetch(io::Error::new(
                io::ErrorKind::InvalidData,
                "failed to decode vault",
            )));
        };

        Ok(Self {
            keys,
            events,
            commands: commands.to_stream().fuse(),
            swarm,
            peer_search,
            search_route: SearchRouteState::Established(search_stream),
            pending_streams: vec![],
            subscriptions: futures::stream::SelectAll::new(),
            temp_buffer,
            nodes,
            username: profile.name,
            vault,
        })
    }

    pub async fn run(mut self) {
        loop {
            futures::select! {
                event = self.swarm.select_next_some() => self.handle_swarm_event(event),
                command = self.commands.select_next_some() => self.handle_command(command),
                search_packet = self.search_route.poll().fuse() => self.handle_search_packet(search_packet),
                (id, request) = self.subscriptions.select_next_some() => self.handle_subscription_request(id, request),
            }
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        todo!()
    }

    fn handle_command(&mut self, event: Command) {
        todo!()
    }

    fn handle_search_packet(&mut self, event: Option<Vec<u8>>) {
        let Some(pckt) = event else {
            let new_route: [_; 3] = self
                .nodes
                .iter()
                .map(|(a, b)| (b.enc.into(), *a))
                .take(3)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap();
            self.search_route = SearchRouteState::Reestablishing(
                self.swarm
                    .behaviour_mut()
                    .onion
                    .open_path(new_route)
                    .expect("to be valid at this point"),
                (),
            );
            return;
        };

        todo!()
    }

    fn handle_subscription_request(&mut self, id: PathId, request: io::Result<Vec<u8>>) {
        let msg = match request {
            Ok(m) => m,
            Err(e) => {
                log::error!("chat subscription error: {e}");
                return;
            }
        };

        let Some(pckt) = Response::decode(&mut msg.as_slice()) else {
            log::error!("chat subscription packet is malformed");
            return;
        };

        let Some(sub) = self.subscriptions.iter_mut().find(|s| s.id == id) else {
            log::error!("subscription not found");
            return;
        };

        match pckt {
            Response::Message(msg) => {
                if msg.chat != sub.chat {
                    log::warn!("message chat does not match subscription");
                    return;
                }

                if msg.proof.is_chat_valid(msg.chat) {
                    log::warn!("message chat is invalid");
                    return;
                }

                let Some(meta) = self.vault.chats.iter_mut().find(|c| c.name == msg.chat) else {
                    log::warn!("message chat not found");
                    return;
                };

                let Some(PrefixedMessage { no: _, content }) = <_>::decode(&mut &*msg.content)
                else {
                    log::warn!("message content is malformed, no message no");
                    return;
                };

                let mut content = content.to_vec();
                let Some(decrypted) = crypto::decrypt(&mut content, meta.secret) else {
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
            Response::FetchedMessages(fmsg @ FetchedMessages { chat, cursor, .. }) => {
                let messages = fmsg.messages.to_vec();
                sub.cursor = cursor;
                (self.events)(Event::FetchedMessages { chat, messages });
            }
            Response::Subscribed(chn) => {
                (self.events)(Event::Subscribed(chn));
            }
            Response::SearchResults(_) => log::error!("unepxected response"),
            Response::ChatNotFound => log::error!("chat not found"),
            Response::FailedMessage(e) => log::error!("failed to put message: {}", e),
            Response::DataRed(_) => log::error!("unexpected response"),
            Response::MailWritten => log::error!("unexpected response"),
            Response::MailWriteFailed(e) => log::error!("failed to write mail: {}", e),
        }
    }

    pub fn username(&self) -> UserName {
        self.username
    }
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
    #[error("failed to search profile: {0}")]
    ProfileSearch(io::Error),
    #[error("failed to fetch vault: {0}")]
    VauldFetch(io::Error),
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

pub async fn run(
    keys: UserKeys,
    chain_bootstrap_node: &str,
    wevents: WriteSignal<Event>,
    rcommands: ReadSignal<Command>,
) {
    let node = Node::new(keys, chain_bootstrap_node, wevents, rcommands)
        .await
        .unwrap();

    // use libp2p::core::multiaddr::Protocol;
    // use libp2p::*;

    // let bootstrap_node: SocketAddr = (Ipv4Addr::LOCALHOST, 8900).into();

    // let nodes = chain_api::nodes("http://localhost:8700")
    //     .await
    //     .unwrap()
    //     .into_iter()
    //     .map(node_data_to_path_seg)
    //     .collect::<HashMap<_, _>>();

    // let keys = UserKeys::new();
    // let keypair = identity::Keypair::ed25519_from_bytes(keys.sign.ed).unwrap();
    // let transport = websocket_websys::Transport::new(100)
    //     .upgrade(Version::V1Lazy)
    //     .authenticate(noise::Config::new(&keypair).unwrap())
    //     .multiplex(yamux::Config::default())
    //     .boxed();
    // let peer_id = keypair.public().to_peer_id();

    // let behaviour = Behaviour {
    //     onion: onion::Behaviour::new(
    //         onion::Config::new(None, peer_id).keep_alive_interval(Duration::from_secs(100)),
    //     ),
    //     kad: kad::Behaviour::with_config(
    //         peer_id,
    //         kad::store::MemoryStore::new(peer_id),
    //         mem::take(
    //             kad::Config::default().set_replication_factor(protocols::chat::REPLICATION_FACTOR),
    //         ),
    //     ),
    // };

    // let mut swarm = swarm::Swarm::new(
    //     transport,
    //     behaviour,
    //     peer_id,
    //     libp2p::swarm::Config::with_wasm_executor(),
    // );

    // swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));

    // // rely on a fact that hash map has random order of iteration (RandomState)
    // let route: [_; 3] = nodes
    //     .iter()
    //     .map(|(a, b)| (*b, *a))
    //     .take(3)
    //     .collect::<Vec<_>>()
    //     .try_into()
    //     .unwrap();

    let mut commands = rcommands.to_stream().fuse();

    // fn dile_addr(addr: SocketAddr) -> Multiaddr {
    //     Multiaddr::empty()
    //         .with(match addr {
    //             SocketAddr::V4(addr) => Protocol::Ip4(addr.ip().to_owned()),
    //             SocketAddr::V6(addr) => Protocol::Ip6(addr.ip().to_owned()),
    //         })
    //         .with(Protocol::Tcp(addr.port()))
    //         .with(Protocol::Ws("/".into()))
    // }

    // swarm.dial(dile_addr(bootstrap_node)).unwrap();

    // let mut discovery = KadPeerSearch::default();
    // let mut peer_search_route = None::<EncryptedStream>;
    // let mut search_route = swarm.behaviour_mut().onion.open_path(route).unwrap();
    // let mut buffer = vec![];
    // let mut pending_streams = vec![];
    // let mut subscriptions = SelectAll::<Subscription>::new();
    // let mut connected = false;
    // loop {
    //     enum LoopEvent<A, B> {
    //         Command(Command),
    //         Event(SwarmEvent<A, B>),
    //         SearchPacket(io::Result<Vec<u8>>),
    //         SubscriptionPacket(io::Result<Vec<u8>>),
    //     }

    //     let search_future = std::future::poll_fn(|cx| match peer_search_route.as_mut() {
    //         Some(s) => s.poll(cx).map_ok(|v| v.to_vec()),
    //         None => Poll::Pending,
    //     });

    //     let e = futures::select! {
    //         command = commands.select_next_some() => LoopEvent::Command(command),
    //         event = swarm.select_next_some() => LoopEvent::Event(event),
    //         packet = search_future.fuse() => LoopEvent::SearchPacket(packet),
    //         packet = subscriptions.select_next_some() => LoopEvent::SubscriptionPacket(packet),
    //     };

    //     match e {
    //         LoopEvent::SubscriptionPacket(r) => {
    //             let msg = match r {
    //                 Ok(m) => m,
    //                 Err(e) => {
    //                     log::error!("chat subscription error: {e}");
    //                     continue;
    //                 }
    //             };

    //             let Some(pckt) = Response::decode(&mut msg.as_slice()) else {
    //                 continue;
    //             };

    //             match pckt {
    //                 Response::Message(msg) => {
    //                     todo!()
    //                 }
    //                 Response::FetchedMessages(fmsg @ FetchedMessages { chat, cursor, .. }) => {
    //                     let messages = fmsg.messages.to_vec();
    //                     subscriptions
    //                         .iter_mut()
    //                         .find(|s| s.chat == chat)
    //                         .unwrap()
    //                         .cursor = cursor;
    //                     events(Event::FetchedMessages { chat, messages });
    //                 }
    //                 Response::Subscribed(chn) => {
    //                     events(Event::Subscribed(chn));
    //                 }
    //                 Response::SearchResults(_) => log::error!("unepxected response"),
    //                 Response::ChatNotFound => log::error!("chat not found"),
    //                 Response::FailedMessage(e) => log::error!("failed to put message: {}", e),
    //                 Response::DataRed(_) => todo!(),
    //                 Response::MailWritten => todo!(),
    //                 Response::MailWriteFailed(e) => log::error!("failed to write mail: {}", e),
    //             }
    //         }
    //         LoopEvent::SearchPacket(r) => {
    //             let msg = match r {
    //                 Ok(m) => m,
    //                 Err(e) => {
    //                     log::error!("search path error: {e}");
    //                     let route: [_; 3] = nodes
    //                         .iter()
    //                         .map(|(a, b)| (*b, *a))
    //                         .take(3)
    //                         .collect::<Vec<_>>()
    //                         .try_into()
    //                         .unwrap();

    //                     search_route = swarm.behaviour_mut().onion.open_path(route).unwrap();
    //                     peer_search_route = None;
    //                     continue;
    //                 }
    //             };

    //             let Some(Response::SearchResults(SearchResult {
    //                 members,
    //                 key: UserOrChat::Chat(chat),
    //             })) = Response::decode(&mut &*msg)
    //             else {
    //                 log::error!("search packet is malformed");
    //                 continue;
    //             };

    //             let Some((peer, key)) = members
    //                 .into_iter()
    //                 .skip(*peer_id.to_bytes().last().unwrap() as usize % 2)
    //                 .find_map(|p| Some((p, *nodes.get(&p)?)))
    //             else {
    //                 log::error!("no member peer found");
    //                 continue;
    //             };

    //             log::info!("peer picked: {} for chat: {}", peer, chat);

    //             let route: [_; 3] = nodes
    //                 .iter()
    //                 .filter(|&(&a, _)| a != peer)
    //                 .map(|(a, b)| (*b, *a))
    //                 .take(2)
    //                 .chain(iter::once((key, peer)))
    //                 .collect::<Vec<_>>()
    //                 .try_into()
    //                 .unwrap();

    //             pending_streams.push((chat, swarm.behaviour_mut().onion.open_path(route).unwrap()));

    //             log::info!("search results");
    //         }
    //         LoopEvent::Command(c) => match c {
    //             _ if peer_search_route.is_none() => log::error!("sarch route not present"),
    //             Command::Subscrbe { chat } => {
    //                 buffer.clear();
    //                 protocols::chat::Request::SearchFor(UserOrChat::Chat(chat)).encode(&mut buffer);
    //                 peer_search_route.as_mut().unwrap().write(&mut buffer);
    //             }
    //             Command::SendMessage {
    //                 chat,
    //                 name,
    //                 content,
    //             } => {
    //                 let Some(sub) = subscriptions.iter_mut().find(|s| s.chat == chat) else {
    //                     log::error!("chat not found");
    //                     continue;
    //                 };

    //                 todo!()
    //             }
    //             Command::FetchMessages(chat) => {
    //                 let sub = subscriptions.iter_mut().find(|s| s.chat == chat).unwrap();

    //                 buffer.clear();
    //                 Request::FetchMessages(FetchMessages {
    //                     chat,
    //                     cursor: sub.cursor,
    //                 })
    //                 .encode(&mut buffer);
    //                 sub.stream.write(&mut buffer);
    //             }
    //             Command::None => {}
    //         },
    //         LoopEvent::Event(event) => match event {
    //             SwarmEvent::ConnectionEstablished {
    //                 peer_id,
    //                 endpoint: ConnectedPoint::Dialer { address, .. },
    //                 ..
    //             } => {
    //                 swarm.behaviour_mut().kad.add_address(&peer_id, address);
    //                 if !mem::replace(&mut connected, true) {
    //                     swarm.behaviour_mut().kad.bootstrap().unwrap();
    //                 }
    //             }
    //             SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest {
    //                 to,
    //             })) => {
    //                 component_utils::handle_conn_request(to, &mut swarm, &mut discovery);
    //             }
    //             SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
    //                 stream,
    //                 id,
    //             ))) if id == search_route => {
    //                 peer_search_route = Some(stream);
    //                 events(Event::Saturated);
    //             }
    //             SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
    //                 mut stream,
    //                 id,
    //             ))) if pending_streams.iter().any(|&(_, pid)| id == pid) => {
    //                 let index = pending_streams
    //                     .iter()
    //                     .position(|&(_, pid)| id == pid)
    //                     .unwrap();
    //                 let (chat, _) = pending_streams.swap_remove(index);
    //                 buffer.clear();
    //                 Request::Subscribe(chat).encode(&mut buffer);
    //                 log::debug!("subscribing to {}", chat);
    //                 stream.write(&mut buffer);
    //                 subscriptions.push(Subscription {
    //                     chat,
    //                     stream,
    //                     cursor: protocols::chat::NO_CURSOR,
    //                 });
    //             }
    //             SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
    //                 if component_utils::try_handle_conn_response(
    //                     &e,
    //                     &mut swarm,
    //                     &mut discovery,
    //                 ) => {}
    //             SwarmEvent::Behaviour(BehaviourEvent::Onion(o)) => {
    //                 panic!("unexpected onion event: {:?}", o)
    //             }
    //             e => logging::log!("{:?}", e),
    //         },
    //     }
    // }
}

struct Subscription {
    id: PathId,
    chat: ChatName,
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
