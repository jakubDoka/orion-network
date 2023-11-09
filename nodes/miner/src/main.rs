#![feature(iter_advance_by)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]

use chain_api::NodeData;
use component_utils::{
    codec::Codec,
    kad::KadPeerSearch,
    libp2p_kad::{
        store::{MemoryStore, RecordStore},
        InboundRequest, QueryId, Quorum, StoreInserts,
    },
    Base128Bytes,
};
use libp2p::{
    core::{multiaddr, muxing::StreamMuxerBox, upgrade::Version},
    futures::{self, StreamExt},
    kad,
    swarm::{ConnectionHandler, NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Transport,
};
use onion::EncryptedStream;
use protocols::chat::*;
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap, HashSet},
    mem,
    net::Ipv4Addr,
    pin::Pin,
    time::Duration,
    usize,
};

struct Miner {
    swarm: libp2p::swarm::Swarm<Behaviour>,
    client_counter: usize,
    peer_discovery: KadPeerSearch,
    clients: futures::stream::SelectAll<Stream>,
    search_queries: Vec<(usize, UserOrChat, Vec<PeerId>, QueryId)>,
    buffer: Vec<u8>,
}

impl Miner {
    async fn new(port: u16, chain_port: u16) -> Self {
        let enc_keys = crypto::enc::KeyPair::new();
        let sig_keys = crypto::sign::KeyPair::new();
        let local_key = libp2p::identity::Keypair::ed25519_from_bytes(sig_keys.ed).unwrap();
        let peer_id = local_key.public().to_peer_id();

        chain_api::register_node(
            format_args!("http://{}:{chain_port}", Ipv4Addr::LOCALHOST),
            NodeData {
                sign: sig_keys.public_key().into(),
                enc: enc_keys.public_key().into(),
                ip: [127, 0, 0, 1],
                port,
            },
        )
        .await
        .unwrap();

        let behaviour = Behaviour {
            onion: onion::Behaviour::new(
                onion::Config::new(enc_keys.clone().into(), peer_id)
                    .max_streams(10)
                    .keep_alive_interval(Duration::from_secs(100)),
            ),
            kad: kad::Behaviour::with_config(
                peer_id,
                ChatStore {
                    chats: Default::default(),
                    mail_boxes: Default::default(),
                    mem: MemoryStore::new(peer_id),
                },
                mem::take(kad::Config::default().set_record_filtering(StoreInserts::FilterBoth)),
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

        if let Some(back_ref) = port.checked_sub(8801) {
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
            client_counter: 0,
            peer_discovery: Default::default(),
            clients: Default::default(),
            search_queries: Default::default(),
            buffer: Default::default(),
        }
    }

    fn try_handle_search_query(&mut self, id: QueryId, peers: &[PeerId], last: bool) -> bool {
        let Some(index) = self.search_queries.iter().position(|&(.., q)| q == id) else {
            return false;
        };

        let (client_id, _, members, ..) = &mut self.search_queries[index];

        let Some(client) = self.clients.iter_mut().find(|c| c.id == *client_id) else {
            self.search_queries.swap_remove(index);
            return true;
        };

        members.extend(peers);

        if !last {
            return true;
        }

        members.sort_unstable();
        members.dedup();

        let (_, key, members, _) = self.search_queries.swap_remove(index);
        match key {
            UserOrChat::User(key) => {
                let req = InitSearchResult { members, key };
                send_response(req, &mut client.inner, &mut self.buffer);
            }
            UserOrChat::Chat(key) => {
                let req = ProfileResponse::Search(ChatSearchResult { members, key });
                send_response(req, &mut client.inner, &mut self.buffer);
            }
        }

        true
    }

    fn handle_put_record_message(&mut self, key: &[u8], msg: ReplicateMessage) {
        let Some(chat) = ChatName::decode(&mut &*key) else {
            log::warn!("failed to decode chat name for message replication");
            return;
        };

        self.publish_message(msg.to_message(chat));
    }

    fn publish_message(&mut self, msg: Message) -> bool {
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .kad
            .store_mut()
            .put_message(msg.chat, msg.to_replicate())
        {
            log::error!("failed to put message: {e:?}");
            return false;
        }

        for client in self
            .clients
            .iter_mut()
            .filter(|c| c.state.is_this_chat(&msg.chat))
        {
            log::error!("sending message to client");
            send_response(ChatResponse::New(msg), &mut client.inner, &mut self.buffer);
        }

        true
    }

    fn handle_put_record(&mut self, record: kad::Record) {
        let Some(pr) = PutRecord::decode(&mut record.value.as_slice()) else {
            log::warn!("failed to decode record payload");
            return;
        };

        match pr {
            PutRecord::Message(msg) => self.handle_put_record_message(record.key.as_ref(), msg),
            PutRecord::Mail(msg) => {
                let Ok(mail_id) = Identity::try_from(record.key.as_ref()) else {
                    return;
                };

                if let Err(e) = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .put_mail(mail_id, msg)
                {
                    log::error!("failed to put notification: {e:?}");
                    return;
                }

                if let Some(stream) = self
                    .clients
                    .iter_mut()
                    .find(|c| c.state.is_this_profile(&mail_id))
                {
                    send_response(
                        ProfileResponse::Mail(msg),
                        &mut stream.inner,
                        &mut self.buffer,
                    );
                }
            }
            PutRecord::WriteData(wd) => {
                if let Err(e) = self.swarm.behaviour_mut().kad.store_mut().write_data(wd) {
                    log::error!("failed to write data: {e:?}");
                }
            }
            PutRecord::CreateChat(CreateChat { name, proof }) => {
                if let Err(e) = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .create_chat(name, proof)
                {
                    log::warn!("failed to create chat: {e:?}");
                }

                log::info!("created chat {name}");
            }
            PutRecord::ChatHistory(_) => todo!(),
        };
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
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest { to })) => {
                component_utils::handle_conn_request(to, &mut self.swarm, &mut self.peer_discovery)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(
                    &e,
                    &mut self.swarm,
                    &mut self.peer_discovery,
                ) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result: kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk { peers, .. })),
                step,
                ..
            })) if self.try_handle_search_query(id, &peers, step.last) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::InboundStream(inner))) => {
                log::warn!("inbound stream");
                self.clients.push(Stream {
                    id: self.client_counter,
                    state: StreamState::Undecided,
                    inner,
                });
                self.client_counter += 1;
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::InboundRequest {
                request:
                    InboundRequest::PutRecord {
                        record: Some(record),
                        ..
                    },
            })) => self.handle_put_record(record),
            e => log::debug!("{e:?}"),
        }
    }

    fn handle_profile_client_message(&mut self, id: usize, req: Vec<u8>, ident: Identity) {
        let Some(req) = ProfileRequest::decode(&mut req.as_slice()) else {
            log::error!("failed to decode request");
            return;
        };

        let stream = self.clients.iter_mut().find(|s| s.id == id).unwrap();

        match req {
            ProfileRequest::Search(chat) => {
                let qid = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_closest_peers(chat.as_bytes().to_vec());
                self.search_queries
                    .push((stream.id, UserOrChat::Chat(chat), vec![], qid));
            }
            ProfileRequest::WriteData(wd) => {
                log::info!("writing data");
                let res = self.swarm.behaviour_mut().kad.store_mut().write_data(wd);
                let soccess = res.is_ok();
                let resp = match res {
                    Ok(()) => ProfileResponse::DataWritten,
                    Err(e) => ProfileResponse::DataWriteFailed(e),
                };
                send_response(resp, &mut stream.inner, &mut self.buffer);

                if !soccess {
                    return;
                }

                self.swarm
                    .behaviour_mut()
                    .kad
                    .put_record(
                        // todo we send identity twice
                        kad::Record {
                            key: wd.proof.pk.to_vec().into(),
                            value: PutRecord::WriteData(wd).to_bytes(),
                            publisher: None,
                            expires: None,
                        },
                        Quorum::N(protocols::chat::REPLICATION_FACTOR),
                    )
                    .unwrap();
            }
            ProfileRequest::Subscribe(req) => {
                if ident != req.pk {
                    log::warn!("client tried to subscribe to a different profile");
                    return;
                }
                let res = self.swarm.behaviour_mut().kad.store_mut().read_mail(req);
                let req = match res {
                    Ok(bytes) => ProfileSubscribeResponse::Success(bytes),
                    Err(e) => ProfileSubscribeResponse::Failure(e),
                };
                send_response(req, &mut stream.inner, &mut self.buffer);
            }
            ProfileRequest::KeepAlive => todo!(),
        }
    }

    fn handle_undecided_client_message(&mut self, id: usize, req: Vec<u8>) {
        let Some(req) = InitRequest::decode(&mut req.as_slice()) else {
            log::error!("failed to decode request");
            return;
        };

        let stream = self.clients.iter_mut().find(|s| s.id == id).unwrap();

        let store = self.swarm.behaviour_mut().kad.store_mut();
        match req {
            InitRequest::Search(profile) => {
                let qid = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_closest_peers(profile.to_vec());
                self.search_queries
                    .push((stream.id, UserOrChat::User(profile), vec![], qid));
            }
            InitRequest::ReadData(identity) => {
                let resp = store.read_data(&identity);

                self.buffer.clear();
                self.buffer.extend_from_slice(resp);
                stream.inner.write(&mut self.buffer);
                stream.state = StreamState::Profile(identity);
            }
            InitRequest::Subscribe(chats) => {
                if chats.is_empty() {
                    log::warn!("client tried to subscribe to no chats");
                    return;
                }
                let mut messages = vec![];
                for &chat in chats.iter() {
                    let resp = if let Some(chat_state) = store.chats.get(&chat) {
                        let cursor =
                            chat_state
                                .messages
                                .fetch(NO_CURSOR, usize::MAX, &mut messages);
                        ChatResponse::Fetched(FetchedMessages {
                            chat,
                            cursor,
                            messages: &messages,
                        })
                    } else {
                        ChatResponse::NotFound
                    };
                    send_response(resp, &mut stream.inner, &mut self.buffer);
                    messages.clear();
                }
                stream.state = StreamState::Chats(chats.into_iter().collect());
            }
            InitRequest::Create(CreateChat { name, proof }) => {
                let resp = match store.create_chat(name, proof) {
                    Err(err) => ChatResponse::CannotCreate(CreateChatErrorData { err, name }),
                    Ok(()) => ChatResponse::Created(name),
                };
                let success = matches!(resp, ChatResponse::Created(_));
                send_response(resp, &mut stream.inner, &mut self.buffer);
                if !success {
                    return;
                }

                self.swarm
                    .behaviour_mut()
                    .kad
                    .put_record(
                        kad::Record {
                            key: name.to_bytes().into(),
                            value: PutRecord::CreateChat(CreateChat { name, proof }).to_bytes(),
                            publisher: None,
                            expires: None,
                        },
                        Quorum::N(protocols::chat::REPLICATION_FACTOR),
                    )
                    .unwrap();
            }
        }
    }

    fn handle_chat_request(&mut self, id: usize, req: Vec<u8>) {
        let Some(req) = ChatRequest::decode(&mut req.as_slice()) else {
            log::error!("failed to decode request");
            return;
        };

        let stream = self.clients.iter_mut().find(|s| s.id == id).unwrap();

        let store = self.swarm.behaviour_mut().kad.store_mut();
        match req {
            ChatRequest::Send(m) => {
                if !self.publish_message(m) {
                    return;
                }

                self.swarm
                    .behaviour_mut()
                    .kad
                    .put_record(
                        kad::Record {
                            key: m.chat.to_bytes().into(),
                            value: PutRecord::Message(m.to_replicate()).to_bytes(),
                            publisher: None,
                            expires: None,
                        },
                        Quorum::N(protocols::chat::REPLICATION_FACTOR),
                    )
                    .unwrap();
            }
            ChatRequest::Fetch(fm) => {
                let mut messages = vec![];
                let resp = if let Some(chat) = store.chats.get(&fm.chat) {
                    let cursor = chat.messages.fetch(fm.cursor, usize::MAX, &mut messages);
                    ChatResponse::Fetched(FetchedMessages {
                        chat: fm.chat,
                        cursor,
                        messages: &messages,
                    })
                } else {
                    ChatResponse::NotFound
                };
                send_response(resp, &mut stream.inner, &mut self.buffer);
            }
            ChatRequest::KeepAlive => {}
        }
    }

    fn handle_client_message(
        &mut self,
        (id, req): <Stream as component_utils::futures::Stream>::Item,
    ) {
        let Ok(req) = req.map_err(|e| log::error!("stream errored with {e:?}")) else {
            return;
        };

        let Some(stream) = self.clients.iter_mut().find(|s| s.id == id) else {
            log::error!("client no longer exists, what?");
            return;
        };

        match stream.state {
            StreamState::Profile(identity) => self.handle_profile_client_message(id, req, identity),
            StreamState::Chats(..) => self.handle_chat_request(id, req),
            StreamState::Undecided => self.handle_undecided_client_message(id, req),
        }
    }

    async fn run(mut self) {
        loop {
            futures::select! {
                e = self.swarm.select_next_some() => self.handle_event(e),
                m = self.clients.select_next_some() => self.handle_client_message(m),
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

#[allow(deprecated)]
type SE = libp2p::swarm::SwarmEvent<
    <Behaviour as NetworkBehaviour>::ToSwarm,
    <<Behaviour as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::Error,
>;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    config::env_config! {
        PORT: u16,
        CHAIN_PORT: u16,
    }

    Miner::new(PORT, CHAIN_PORT).await.run().await;
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    onion: onion::Behaviour,
    kad: kad::Behaviour<ChatStore>,
    identfy: libp2p::identify::Behaviour,
}

component_utils::impl_kad_search!(Behaviour => (ChatStore, onion::Behaviour => onion, kad));

#[derive(Default)]
struct ChatState {
    messages: MessageBlob,
    members: Vec<Member>,
}

#[derive(Default)]
struct Profie {
    messages: MailBlob,
    action_no: ActionNo,
    data: Vec<u8>,
}

struct ChatStore {
    chats: HashMap<ChatName, ChatState>,
    mail_boxes: HashMap<Identity, Profie>,
    mem: MemoryStore,
}

impl ChatStore {
    fn put_mail(&mut self, id: Identity, msg: &[u8]) -> Result<(), PutMailError> {
        if msg.len() > protocols::chat::MAX_MAIL_SIZE {
            return Err(PutMailError::MailTooBig);
        }

        let mail = self.mail_boxes.entry(id).or_default();

        if !mail.messages.push(msg.iter().copied()) {
            return Err(PutMailError::MailboxFull);
        }

        Ok(())
    }

    fn read_mail(&mut self, req: ActionProof) -> Result<&mut [u8], ReadMailError> {
        if !req.is_profile_valid() {
            return Err(ReadMailError::InvalidProof);
        }

        let mail = self.mail_boxes.entry(req.pk).or_default();

        if mail.action_no >= req.no {
            Err(ReadMailError::NotPermitted)
        } else {
            mail.action_no = req.no;
            Ok(mail.messages.read())
        }
    }

    fn read_data(&mut self, identity: &Identity) -> &[u8] {
        self.mail_boxes
            .get(identity)
            .map(|m| m.data.as_slice())
            .unwrap_or_default()
    }

    fn write_data(&mut self, req: WriteData) -> Result<(), WriteDataError> {
        let mail = self.mail_boxes.entry(req.proof.pk).or_default();

        if !req.proof.is_profile_valid() {
            return Err(WriteDataError::InvalidProof);
        }

        if mail.action_no >= req.proof.no {
            return Err(WriteDataError::NotPermitted);
        }

        mail.data.clear();
        mail.data.extend(req.data);

        Ok(())
    }

    fn put_message(&mut self, chat: ChatName, rm: ReplicateMessage) -> Result<(), PutMessageError> {
        if !rm.proof.is_chat_valid(chat) {
            return Err(PutMessageError::InvalidMessage);
        }

        let Some(chat) = self.chats.get_mut(&chat) else {
            return Err(PutMessageError::ChatNotFound);
        };

        let Some(member) = chat.members.iter_mut().find(|m| m.identity == rm.proof.pk) else {
            return Err(PutMessageError::NotMember);
        };

        if member.action_no >= rm.proof.no {
            return Err(PutMessageError::MessageNumberTooLow);
        }

        member.action_no = rm.proof.no;
        let Some(msg) = MessagePayload::decode(&mut &*rm.content) else {
            return Err(PutMessageError::InvalidContent);
        };

        match msg {
            MessagePayload::Arbitrary(content)
                if content.len() > protocols::chat::MAX_MESSAGE_SIZE =>
            {
                return Err(PutMessageError::MessageTooBig);
            }
            MessagePayload::Arbitrary(content) => chat
                .messages
                .push(Base128Bytes::new(member.id as _).chain(content.iter().copied())),
            MessagePayload::AddMember(am) => {
                let issuer_perm = member.perm;
                let Some(member) = chat.members.iter_mut().find(|m| m.identity == am.invited)
                else {
                    let id = chat.members.len() as _;
                    chat.members.push(Member {
                        id,
                        identity: am.invited,
                        perm: issuer_perm + am.perm_offset,
                        action_no: 0,
                    });
                    return Ok(());
                };

                if member.perm <= issuer_perm {
                    return Err(PutMessageError::NotPermitted);
                }

                member.perm += issuer_perm + am.perm_offset;
            }
            MessagePayload::RemoveMember(id) => {
                let issuer_perm = member.perm;
                let Ok(index) = chat.members.binary_search_by_key(&id, |m| m.id) else {
                    return Err(PutMessageError::MemberNotFound);
                };

                let member = &mut chat.members[index];
                if issuer_perm >= member.perm {
                    return Err(PutMessageError::NotPermitted);
                }

                chat.members.remove(index);
            }
        }

        Ok(())
    }

    fn create_chat(&mut self, name: ChatName, proof: ActionProof) -> Result<(), CreateChatError> {
        if !proof.is_chat_valid(name) || proof.no != 0 {
            log::warn!("invalid proof for chat creation");
            return Err(CreateChatError::InvalidProof);
        }

        let Entry::Vacant(chat) = self.chats.entry(name) else {
            log::warn!("chat already exists");
            return Err(CreateChatError::AlreadyExists);
        };

        chat.insert(ChatState::default()).members.push(Member {
            id: 0,
            identity: proof.pk,
            perm: 0,
            action_no: 0,
        });

        Ok(())
    }
}

impl RecordStore for ChatStore {
    type RecordsIter<'a> = std::iter::Map<std::collections::hash_map::Iter<'a, ChatName, ChatState>,
        fn((&ChatName, &ChatState)) -> Cow<'a, kad::Record>> where Self: 'a;
    type ProvidedIter<'a> = <MemoryStore as RecordStore>::ProvidedIter<'a> where Self: 'a;

    fn get(&self, _: &kad::RecordKey) -> Option<std::borrow::Cow<'_, kad::Record>> {
        None
    }

    fn put(&mut self, _: kad::Record) -> kad::store::Result<()> {
        Ok(())
    }

    fn remove(&mut self, _: &kad::RecordKey) {}

    fn records(&self) -> Self::RecordsIter<'_> {
        // TODO: replicate only keys and make nodes fetch the data from the sender if they dont have it
        self.chats.iter().map(|(k, v)| {
            Cow::Owned(kad::Record {
                key: k.to_bytes().into(),
                value: v.messages.as_vec(),
                publisher: None,
                expires: None,
            })
        })
    }

    fn add_provider(&mut self, r: kad::ProviderRecord) -> kad::store::Result<()> {
        self.mem.add_provider(r)
    }

    fn providers(&self, r: &kad::RecordKey) -> Vec<kad::ProviderRecord> {
        self.mem.providers(r)
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.mem.provided()
    }

    fn remove_provider(&mut self, k: &kad::RecordKey, p: &PeerId) {
        self.mem.remove_provider(k, p)
    }
}

struct Stream {
    id: usize,
    state: StreamState,
    inner: EncryptedStream,
}

#[allow(clippy::large_enum_variant)]
enum StreamState {
    Profile(Identity),
    Chats(HashSet<ChatName>),
    Undecided,
}

impl StreamState {
    fn is_this_profile(&self, id: &Identity) -> bool {
        matches!(self, Self::Profile(other) if other == id)
    }

    fn is_this_chat(&self, chats: &ChatName) -> bool {
        matches!(self, Self::Chats(other) if other.contains(chats))
    }
}

impl futures::Stream for Stream {
    type Item = (usize, Result<Vec<u8>, std::io::Error>);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|p| p.map(|p| (self.id, p)))
    }
}
