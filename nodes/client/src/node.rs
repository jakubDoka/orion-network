use crate::BootPhase;
use component_utils::futures::stream::Fuse;
use component_utils::kad::KadPeerSearch;
use component_utils::Codec;
use component_utils::{futures, LinearMap};
use crypto::decrypt;
use leptos::signal_prelude::*;
use libp2p::core::upgrade::Version;
use libp2p::futures::StreamExt;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::*;
use libp2p::{PeerId, Swarm};
use onion::{EncryptedStream, PathId};
use protocols::chat::*;
use protocols::contracts::{NodeData, UserData, UserIdentity};
use rand::seq::IteratorRandom;
use std::collections::{HashMap, HashSet};
use std::task::Poll;
use std::time::Duration;
use std::u8;
use std::{io, mem, pin::Pin, usize};
use web_sys::wasm_bindgen::JsValue;

pub type MessageContent = std::rc::Rc<str>;

component_utils::protocol! { 'a:
    #[derive(Default)]
    struct Vault {
        chats: LinearMap<ChatName, ChatMeta>,
        theme: Theme,
        action_no: ActionNo,
    }

    struct ChatMeta {
        secret: crypto::SharedSecret,
        action_no: ActionNo,
        permission: Permission,
    }

    struct RawChatMessage<'a> {
        user: UserName,
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

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    NewMessage {
        chat: ChatName,
        name: UserName,
        content: MessageContent,
    },
    FetchedMessages {
        chat: ChatName,
        messages: Vec<(UserName, MessageContent)>,
        end: bool,
    },
    AddedMember(AddMember),
    ChatCreated(ChatName),
    CannotCreateChat(CreateChatErrorData),
    MailWritten,
    MailWriteError(WriteMailError),
    None,
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Command {
    #[allow(dead_code)]
    SendMessage {
        chat: ChatName,
        content: String,
    },
    CreateChat(ChatName),
    InviteUser {
        chat: ChatName,
        user: UserData,
    },
    #[allow(dead_code)]
    FetchMessages(ChatName, bool),
    SetTheme(Theme),
    None,
}

pub struct Node {
    events: WriteSignal<Event>,
    commands: Fuse<Pin<Box<dyn futures::Stream<Item = Command>>>>,
    keys: UserKeys,
    swarm: Swarm<Behaviour>,
    peer_search: KadPeerSearch,
    profile_path: EncryptedStream,
    subscriptions: futures::stream::SelectAll<Subscription>,
    pending_subscriptions: Vec<(PathId, SubIntent)>,
    nodes: HashMap<PeerId, NodeData>,
    buffer: Vec<u8>,
    buffer2: Vec<u8>,
    vault: Vault,
}

#[allow(clippy::large_enum_variant)]
pub enum SubIntent {
    Create(ChatName, ActionProof),
    Invited(ChatName),
}

impl Node {
    pub async fn new(
        keys: UserKeys,
        events: WriteSignal<Event>,
        commands: ReadSignal<Command>,
        wboot_phase: WriteSignal<Option<BootPhase>>,
    ) -> Result<Self, BootError> {
        wboot_phase(Some(BootPhase::FetchTopology));

        let chain_api = crate::chain_node(keys.name).await.unwrap();
        let node_request = chain_api.list(crate::node_contract());
        let profile_request = chain_api.get_profile_by_name(crate::user_contract(), keys.name);
        log::debug!("{:?}", keys.name);
        let (node_data, profile) = futures::join!(node_request, profile_request);
        let (node_data, profile) = (
            node_data.map_err(BootError::FetchNodes)?,
            profile.map_err(BootError::FetchProfile)?,
        );
        let profile = UserIdentity::from(profile);

        assert_eq!(
            profile.sign,
            crypto::sign::SerializedPublicKey::from(keys.sign.public_key())
        );

        let nodes = node_data
            .into_iter()
            .map(|n| chain_api.get_by_identity(crate::node_contract(), n))
            .collect::<futures::stream::FuturesUnordered<_>>()
            .filter_map(|n| async move { n.ok() })
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(NodeData::from)
            .map(node_data_to_path_seg)
            .collect::<HashMap<_, _>>();

        if nodes.len() < MIN_NODES {
            return Err(BootError::NotEnoughNodes(nodes.len()));
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
                    kad::Config::default()
                        .set_replication_factor(protocols::chat::REPLICATION_FACTOR),
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

        let route @ [(enter_node, ..), _, _] = nodes
            .iter()
            .map(|(a, b)| (*b, *a))
            .take(3)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));
        use libp2p::core::multiaddr::Protocol;
        swarm
            .dial(
                Multiaddr::empty()
                    .with(Protocol::Ip4(enter_node.ip.into()))
                    .with(Protocol::Tcp(enter_node.port + 100)) // uh TODO
                    .with(Protocol::Ws("/".into())),
            )
            .unwrap();

        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
            })) = swarm.select_next_some().await
            {
                if let Some(addr) = info.listen_addrs.first() {
                    swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer_id, addr.clone());
                    break;
                }
            }
        }

        wboot_phase(Some(BootPhase::InitialRoute));

        let mut peer_search = KadPeerSearch::default();

        let pid = swarm
            .behaviour_mut()
            .onion
            .open_path(route.map(|(u, p)| (u.enc.into(), p)))
            .unwrap();
        let mut init_stream = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                    _,
                ))) if id == pid => break stream,
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                e => log::error!("{:?}", e),
            }
        };

        wboot_phase(Some(BootPhase::ProfileSearch));

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

        log::debug!("members: {members:#?}");

        let Some(pick) = members.into_iter().choose(&mut rand::thread_rng()) else {
            todo!("error handling")
        };

        wboot_phase(Some(BootPhase::ProfileLoad));

        let route = pick_route(&nodes, pick);
        let pid = swarm.behaviour_mut().onion.open_path(route).unwrap();
        let mut profile_stream = loop {
            match swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                    _,
                ))) if id == pid => break stream,
                e if Self::try_handle_common_event(&e, &mut swarm, &mut peer_search) => {}
                e => log::error!("{:?}", e),
            }
        };

        send_request(
            InitRequest::ReadData(profile.sign),
            &mut profile_stream,
            &mut buffer,
        );

        let mut resp = profile_stream.next().await.unwrap().unwrap();
        let mut vault = if resp.is_empty() {
            Vault {
                action_no: 1,
                ..Default::default()
            }
        } else {
            let vault = decrypt(&mut resp, keys.vault).unwrap();
            Vault::decode(&mut &*vault).unwrap()
        };

        let _ = vault.theme.apply();

        let mut mail;
        let bytes = loop {
            send_request(
                ProfileRequest::Subscribe(ActionProof::for_profile(
                    &mut vault.action_no,
                    &keys.sign,
                )),
                &mut profile_stream,
                &mut buffer,
            );
            mail = profile_stream.next().await.unwrap().unwrap();
            let Some(resp) = <_>::decode(&mut &*mail) else {
                todo!("error handling");
            };
            break match resp {
                ProfileSubscribeResponse::Success(b) => b,
                ProfileSubscribeResponse::Failure(ReadMailError::NotPermitted) => {
                    continue;
                }
                ProfileSubscribeResponse::Failure(e) => {
                    todo!("error handling: {e}");
                }
            };
        };

        wboot_phase(Some(BootPhase::ChatSearch));

        for &chat in vault.chats.keys() {
            send_request(
                ProfileRequest::Search(chat),
                &mut profile_stream,
                &mut buffer,
            );
        }

        let mut awaiting = vec![];

        log::debug!("{}", vault.chats.len());

        let mut topology = HashMap::<PeerId, HashSet<ChatName>>::new();
        let mut discovered = 0;
        for _ in 0..vault.chats.len() {
            let resp = match profile_stream.next().await.unwrap() {
                Ok(resp) => resp,
                Err(e) => return Err(BootError::ChatSearch(e)),
            };

            let Some(res) = <_>::decode(&mut &*resp) else {
                log::error!("search response is malformed");
                continue;
            };

            let ProfileResponse::Search(ChatSearchResult { members, key }) = res else {
                log::error!("search response is of invalid variant");
                continue;
            };

            log::debug!("search response: {key} {members:#?}");
            if !vault.chats.contains_key(&key) {
                log::error!("search to unexistent chat");
                continue;
            }

            discovered += 1;

            for member in members {
                topology.entry(member).or_default().insert(key);
            }
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

        to_connect
            .into_iter()
            .map(|(pick, set)| {
                let route = pick_route(&nodes, pick);
                let pid = swarm.behaviour_mut().onion.open_path(route).unwrap();
                (pid, pick, set)
            })
            .collect_into(&mut awaiting);

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

            send_request(
                InitRequest::Subscribe(ChatSubs {
                    chats: subs.iter().copied().collect(),
                    identity: keys.sign.public_key().into(),
                }),
                &mut stream,
                &mut buffer,
            );

            subscriptions.push(Subscription {
                id,
                peer_id,
                subs,
                stream,
                cursor: protocols::chat::NO_CURSOR,
            });
        }

        wboot_phase(Some(BootPhase::ChatRun));

        let mut s = Self {
            keys,
            events,
            commands: commands.to_stream().fuse(),
            swarm,
            peer_search,
            profile_path: profile_stream,
            subscriptions,
            nodes,
            pending_subscriptions: vec![],
            buffer,
            buffer2: vec![],
            vault,
        };

        for mail in unpack_messages(bytes) {
            s.handle_mail(mail);
        }

        Ok(s)
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
                command = self.commands.select_next_some() => self.handle_command(command),
                search_packet = self.profile_path.select_next_some() => self.handle_profile_response(search_packet),
                (id, response) = self.subscriptions.select_next_some() => self.handle_subscription_response(id, response),
            }
        }
    }

    fn handle_mail(&mut self, bytes: &[u8]) {
        let mail = match Mail::decode(&mut &*bytes) {
            Some(m) => m,
            None => {
                log::error!("mail is malformed");
                return;
            }
        };

        match mail {
            Mail::ChatInvite(invite) => {
                if self.vault.chats.contains_key(&invite.chat) {
                    log::error!("chat already exists");
                    return;
                }

                let chat = ChatMeta {
                    secret: self
                        .keys
                        .enc
                        .decapsulate_choosen(invite.secret.into())
                        .unwrap(),
                    action_no: ActionNo::MAX,
                    permission: Permission::default(),
                };

                self.vault.chats.insert(invite.chat, chat);

                send_request(
                    ProfileRequest::Search(invite.chat),
                    &mut self.profile_path,
                    &mut self.buffer,
                );

                self.save_vault();
            }
        }
    }

    fn handle_profile_response(&mut self, packet: io::Result<Vec<u8>>) {
        let packet = match packet {
            Ok(p) => p,
            Err(e) => {
                log::error!("profile response error: {e}");
                return;
            }
        };

        let Some(resp) = ProfileResponse::decode(&mut packet.as_slice()) else {
            log::error!("profile response is malformed");
            return;
        };

        match resp {
            ProfileResponse::Mail(bytes) => self.handle_mail(bytes),
            ProfileResponse::DataWritten => log::debug!("vault written"),
            ProfileResponse::DataWriteFailed(e) => log::error!("vault write failed: {e}"),
            ProfileResponse::Search(ChatSearchResult { members, key }) => {
                let Some(meta) = self.vault.chats.get_mut(&key) else {
                    log::error!("search to unexistent chat");
                    return;
                };

                if let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| members.contains(&s.peer_id))
                {
                    let req = ChatRequest::OtherInit(if meta.action_no == 0 {
                        InitRequest::Create(CreateChat {
                            name: key,
                            proof: ActionProof::for_chat(&mut meta.action_no, &self.keys.sign, key),
                        })
                    } else if meta.action_no == ActionNo::MAX {
                        InitRequest::Subscribe(ChatSubs {
                            chats: [key].into(),
                            identity: self.keys.sign.public_key().into(),
                        })
                    } else {
                        panic!("{key}");
                    });
                    send_request(req, &mut sub.stream, &mut self.buffer);
                    sub.subs.insert(key);
                    return;
                };

                let Some(pick) = members.into_iter().choose(&mut rand::thread_rng()) else {
                    todo!("error handling")
                };

                let route = pick_route(&self.nodes, pick);
                let quid = self.swarm.behaviour_mut().onion.open_path(route).unwrap();

                let intent = if meta.action_no == 0 {
                    SubIntent::Create(
                        key,
                        ActionProof::for_chat(&mut meta.action_no, &self.keys.sign, key),
                    )
                } else if meta.action_no == ActionNo::MAX {
                    SubIntent::Invited(key)
                } else {
                    panic!("{key}");
                };

                self.pending_subscriptions.push((quid, intent));
            }
            ProfileResponse::MailWritten => (self.events)(Event::MailWritten),
            ProfileResponse::MailWriteFailed(e) => (self.events)(Event::MailWriteError(e)),
        }
    }

    fn handle_swarm_event(&mut self, event: SE) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                mut stream,
                id,
                peer_id,
            ))) => {
                let Some(index) = self
                    .pending_subscriptions
                    .iter()
                    .position(|&(pid, ..)| pid == id)
                else {
                    return;
                };
                let (.., intent) = self.pending_subscriptions.swap_remove(index);

                let (req, name) = match intent {
                    SubIntent::Create(name, proof) => {
                        (InitRequest::Create(CreateChat { name, proof }), name)
                    }
                    SubIntent::Invited(name) => (
                        InitRequest::Subscribe(ChatSubs {
                            chats: [name].into(),
                            identity: self.keys.sign.public_key().into(),
                        }),
                        name,
                    ),
                };
                send_request(req, &mut stream, &mut self.buffer);
                self.subscriptions.push(Subscription {
                    id,
                    peer_id,
                    subs: [name].into(),
                    stream,
                    cursor: NO_CURSOR,
                });
            }
            e if Self::try_handle_common_event(&e, &mut self.swarm, &mut self.peer_search) => {}
            e => log::debug!("{:?}", e),
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::SendMessage { chat, content } => {
                let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| s.subs.contains(&chat))
                else {
                    log::error!("chat not found when sending message");
                    return;
                };

                let Some(meta) = self.vault.chats.get_mut(&chat) else {
                    log::error!("chat meta not found when sending messxge");
                    return;
                };

                self.buffer.clear();
                RawChatMessage {
                    user: self.keys.name,
                    content: &content,
                }
                .encode(&mut self.buffer);
                crypto::encrypt(&mut self.buffer, meta.secret);

                self.buffer2.clear();
                MessagePayload::Arbitrary(&self.buffer).encode(&mut self.buffer2);

                send_request(
                    ChatRequest::Send(Message {
                        chat,
                        content: &self.buffer2,
                        proof: ActionProof::for_chat(&mut meta.action_no, &self.keys.sign, chat),
                    }),
                    &mut sub.stream,
                    &mut self.buffer,
                );
            }
            Command::InviteUser { chat, user } => {
                let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| s.subs.contains(&chat))
                else {
                    log::error!("chat not found when sending message");
                    return;
                };

                let Some(meta) = self.vault.chats.get_mut(&chat) else {
                    log::error!("chat meta not found when sending messxge");
                    return;
                };

                let payload = MessagePayload::AddMember(AddMember {
                    invited: user.sign,
                    perm_offset: 0,
                });

                self.buffer.clear();
                payload.encode(&mut self.buffer);
                send_request(
                    ChatRequest::Send(Message {
                        chat,
                        content: &self.buffer,
                        proof: ActionProof::for_chat(&mut meta.action_no, &self.keys.sign, chat),
                    }),
                    &mut sub.stream,
                    &mut self.buffer2,
                );

                let mail = Mail::ChatInvite(ChatInvite {
                    chat,
                    member_id: 0,
                    secret: self
                        .keys
                        .enc
                        .encapsulate_choosen(&user.enc.into(), meta.secret)
                        .unwrap()
                        .into(),
                });

                self.buffer.clear();
                mail.encode(&mut self.buffer);
                send_request(
                    ProfileRequest::SendMail(SendMail {
                        id: user.sign,
                        data: &self.buffer,
                    }),
                    &mut self.profile_path,
                    &mut self.buffer2,
                );
            }
            Command::CreateChat(name) => {
                let meta = ChatMeta {
                    secret: crypto::new_secret(),
                    action_no: 0,
                    permission: 0,
                };

                self.vault.chats.insert(name, meta);

                send_request(
                    ProfileRequest::Search(name),
                    &mut self.profile_path,
                    &mut self.buffer,
                );
            }
            Command::FetchMessages(chat, restart) => {
                let Some(sub) = self
                    .subscriptions
                    .iter_mut()
                    .find(|s| s.subs.contains(&chat))
                else {
                    log::error!("chat not found when sending message");
                    return;
                };

                send_request(
                    ChatRequest::Fetch(FetchMessages {
                        chat,
                        cursor: if restart { NO_CURSOR } else { sub.cursor },
                    }),
                    &mut sub.stream,
                    &mut self.buffer,
                );
            }
            Command::SetTheme(theme) => {
                if self.vault.theme != theme {
                    self.vault.theme = theme;
                    self.save_vault();
                }
            }
            Command::None => {}
        }
    }

    fn handle_subscription_response(&mut self, id: PathId, request: io::Result<Vec<u8>>) {
        let Ok(msg) = request.inspect_err(|e| log::error!("chat subscription error: {e}")) else {
            return;
        };

        let Some(pckt) = ChatResponse::decode(&mut msg.as_slice()) else {
            log::error!("chat subscription packet is malformed, {msg:?}");
            return;
        };

        let Some(sub) = self.subscriptions.iter_mut().find(|s| s.id == id) else {
            log::error!("subscription not found");
            return;
        };

        match pckt {
            ChatResponse::New(msg) => {
                let Some(meta) = self.vault.chats.get(&msg.chat) else {
                    log::warn!("message chat does not match subscription");
                    return;
                };

                if !msg.proof.is_chat_valid(msg.chat) {
                    log::warn!("message chat is invalid");
                    return;
                }

                let Some(payload) = MessagePayload::decode(&mut &*msg.content) else {
                    log::warn!("message content is malformed");
                    return;
                };

                match payload {
                    MessagePayload::Arbitrary(content) => {
                        let mut content = content.to_vec();
                        let Some(decrypted) = crypto::decrypt(&mut content, meta.secret) else {
                            log::warn!("message content is malformed, cannot decrypt");
                            return;
                        };

                        let Some(RawChatMessage { user, content }) = <_>::decode(&mut &*decrypted)
                        else {
                            log::warn!("message is decriptable but still malformed");
                            return;
                        };

                        (self.events)(Event::NewMessage {
                            chat: msg.chat,
                            name: user,
                            content: content.into(),
                        });
                    }
                    MessagePayload::AddMember(a) => (self.events)(Event::AddedMember(a)),
                    MessagePayload::RemoveMember(_) => todo!(),
                }
            }
            ChatResponse::Subscribed(Subscribed { chat, no }) => {
                let Some(meta) = self.vault.chats.get_mut(&chat) else {
                    log::warn!("message chat does not match subscription");
                    return;
                };
                log::debug!("subscribed to {chat} {no}");
                if meta.action_no == ActionNo::MAX {
                    (self.events)(Event::ChatCreated(chat));
                }
                meta.action_no = no + 1;
            }
            ChatResponse::Fetched(FetchedMessages {
                chat,
                cursor,
                messages,
            }) => {
                let Some(meta) = self.vault.chats.get_mut(&chat) else {
                    log::warn!("message chat does not match subscription");
                    return;
                };

                let messages = unpack_messages(messages)
                    .filter_map(|m| {
                        let Some(PrefixedMessage { prefix: _, content }) = <_>::decode(&mut &*m)
                        else {
                            log::warn!("message does not have prefix");
                            return None;
                        };

                        self.buffer.clear();
                        self.buffer.extend_from_slice(content.0);

                        let Some(decrypted) = crypto::decrypt(&mut self.buffer, meta.secret) else {
                            log::warn!("message content is malformed, cannot decrypt");
                            return None;
                        };

                        let Some(RawChatMessage { user, content }) = <_>::decode(&mut &*decrypted)
                        else {
                            log::warn!("message is decriptable but still malformed");
                            return None;
                        };

                        Some((user, content.into()))
                    })
                    .collect::<Vec<_>>();

                let end = messages.len() < MESSAGE_FETCH_LIMIT && cursor == NO_CURSOR;

                sub.cursor = cursor;
                (self.events)(Event::FetchedMessages {
                    chat,
                    messages,
                    end,
                });
            }
            ChatResponse::Created(ch) => {
                self.save_vault();
                log::debug!("created chat {ch}");
                (self.events)(Event::ChatCreated(ch))
            }

            ChatResponse::NotFound => log::error!("chat not found"),
            ChatResponse::Failed(e) => log::error!("failed to fetch messages: {}", e),
            ChatResponse::CannotCreate(e) => (self.events)(Event::CannotCreateChat(e)),
        }
    }

    fn save_vault(&mut self) {
        log::debug!("saving vault");
        let proof = ActionProof::for_profile(&mut self.vault.action_no, &self.keys.sign);
        self.buffer.clear();
        self.vault.encode(&mut self.buffer);
        crypto::encrypt(&mut self.buffer, self.keys.vault);
        send_request(
            ProfileRequest::WriteData(WriteData {
                data: &self.buffer,
                proof,
            }),
            &mut self.profile_path,
            &mut self.buffer2,
        );
    }

    pub fn username(&self) -> UserName {
        self.keys.name
    }

    pub fn chats(&self) -> impl Iterator<Item = ChatName> + '_ {
        self.vault.chats.keys().copied()
    }
}

fn pick_route(
    nodes: &HashMap<PeerId, NodeData>,
    target: PeerId,
) -> [(onion::PublicKey, PeerId); 3] {
    assert!(nodes.len() >= 2);
    let mut rng = rand::thread_rng();
    let mut picked = nodes
        .iter()
        .filter(|(p, _)| **p != target)
        .map(|(p, ud)| (ud.enc.into(), *p))
        .choose_multiple(&mut rng, 2);
    picked.insert(0, (nodes.get(&target).unwrap().enc.into(), target));
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
    id: PathId, // we still keep if for faster comparison
    peer_id: PeerId,
    subs: HashSet<ChatName>,
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
    identify: libp2p::identify::Behaviour,
}

component_utils::impl_kad_search!(Behaviour => (onion::Behaviour => onion));
