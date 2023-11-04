#![feature(iter_advance_by)]
#![feature(if_let_guard)]
#![feature(map_try_insert)]

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    mem,
    net::Ipv4Addr,
    pin::Pin,
    time::Duration,
    u32, usize, vec,
};

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
    swarm::{NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Transport,
};
use onion::EncryptedStream;
use protocols::chat::*;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    config::env_config! {
        PORT: u16,
        CHAIN_PORT: u16,
    }

    let enc_keys = crypto::enc::KeyPair::new();
    let sig_keys = crypto::sign::KeyPair::new();
    let local_key = libp2p::identity::Keypair::ed25519_from_bytes(sig_keys.ed).unwrap();
    let peer_id = local_key.public().to_peer_id();

    chain_api::register_node(
        format_args!("http://{}:{CHAIN_PORT}", Ipv4Addr::LOCALHOST),
        NodeData {
            sign: sig_keys.public_key().into(),
            enc: enc_keys.public_key().into(),
        },
    )
    .await
    .unwrap();

    let behaviour = Behaviour {
        onion: onion::Behaviour::new(
            onion::Config::new(enc_keys.into(), peer_id)
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
    .upgrade(Version::V1Lazy)
    .authenticate(libp2p::noise::Config::new(&local_key).unwrap())
    .multiplex(libp2p::yamux::Config::default())
    .or_transport(
        libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default())
            .upgrade(Version::V1Lazy)
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
        libp2p::swarm::Config::with_tokio_executor().with_idle_connection_timeout(Duration::MAX),
    );

    swarm
        .listen_on(
            Multiaddr::empty()
                .with(multiaddr::Protocol::Ip4([0; 4].into()))
                .with(multiaddr::Protocol::Tcp(PORT)),
        )
        .unwrap();

    swarm
        .listen_on(
            Multiaddr::empty()
                .with(multiaddr::Protocol::Ip4([0; 4].into()))
                .with(multiaddr::Protocol::Tcp(PORT + 100))
                .with(multiaddr::Protocol::Ws("/".into())),
        )
        .unwrap();

    // very fucking important
    swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    if let Some(back_ref) = PORT.checked_sub(8801) {
        swarm
            .dial(
                Multiaddr::empty()
                    .with(multiaddr::Protocol::Ip4(Ipv4Addr::LOCALHOST))
                    .with(multiaddr::Protocol::Tcp(back_ref + 8800)),
            )
            .unwrap();
    }

    let mut client_ids = 0;
    let mut peer_discovery = KadPeerSearch::default();
    let mut clients = futures::stream::SelectAll::<Stream>::new();
    let mut search_queries = VecDeque::<(usize, UserOrChat, Vec<PeerId>, QueryId)>::new();
    let mut buffer = Vec::<u8>::new();
    loop {
        let e = futures::select! {
            e = swarm.select_next_some() => Ok(e),
            m = clients.select_next_some() => Err(m),
        };

        let e = match e {
            Ok(e) => e,
            Err((i, m)) => {
                log::debug!("received client message");
                let m = match m {
                    Ok(m) => m,
                    Err(e) => {
                        log::error!("stream closed with error: {e}");
                        continue;
                    }
                };
                let Some(msg) = <Request as Codec>::decode(&mut m.as_slice()) else {
                    log::error!("failed to decode msg");
                    continue;
                };

                let Some(stream) = clients.iter_mut().find(|s| s.id == i) else {
                    log::info!("client no longer exists");
                    continue;
                };

                let record = match msg {
                    Request::Subscribe(to) => {
                        log::debug!("subscription from {i} to {to:?}");
                        stream.subscribed.insert(to);
                        buffer.clear();
                        Response::Subscribed(to).encode(&mut buffer);
                        stream.inner.write(&mut buffer);
                        continue;
                    }
                    Request::Send(m) => {
                        log::debug!("message from {i} to {:?}", m.chat);
                        let msg = ReplicateMessage {
                            content: m.content,
                            content_sig: m.content_sig,
                            sender: m.sender,
                        };

                        if let Err(e) = swarm
                            .behaviour_mut()
                            .kad
                            .store_mut()
                            .put_message(m.chat, msg)
                        {
                            log::info!("failed to put message: {e:?}");
                            stream.inner.write(&mut [e as u8]);
                            continue;
                        }

                        let chat = m.chat;
                        for client in clients.iter_mut().filter(|c| c.subscribed.contains(&chat)) {
                            buffer.clear();
                            <Response as Codec>::encode(&Response::Message(m), &mut buffer);
                            client.inner.write(&mut buffer);
                        }

                        kad::Record {
                            key: m.chat.to_bytes().into(),
                            value: PutRecord::Message(msg).to_bytes(),
                            publisher: None,
                            expires: None,
                        }
                    }
                    Request::FetchMessages(fm) => {
                        let mut messages = vec![];
                        let resp = if let Some(chat) =
                            swarm.behaviour_mut().kad.store_mut().chats.get(&fm.chat)
                        {
                            let cursor = chat.messages.fetch(fm.cursor, usize::MAX, &mut messages);
                            Response::FetchedMessages(FetchedMessages {
                                chat: fm.chat,
                                cursor,
                                messages: &messages,
                            })
                        } else {
                            Response::ChatNotFound
                        };
                        buffer.clear();
                        resp.encode(&mut buffer);
                        stream.inner.write(&mut buffer);
                        continue;
                    }
                    Request::KeepAlive => continue,
                    Request::SearchFor(key) => {
                        let qid = swarm
                            .behaviour_mut()
                            .kad
                            .get_closest_peers(key.as_slice().to_vec());
                        search_queries.push_back((i, key, vec![], qid));
                        continue;
                    }
                    Request::ReadMail(_) => todo!(),
                    Request::WriteData(_) => todo!(),
                    Request::ReadData(req) => {
                        let data = swarm.behaviour_mut().kad.store_mut().read_data(req);
                        buffer.clear();
                        Response::DataRed(data).encode(&mut buffer);
                        stream.inner.write(&mut buffer);
                        continue;
                    }
                    Request::WriteMail(req) => {
                        let err = swarm
                            .behaviour_mut()
                            .kad
                            .store_mut()
                            .put_mail(req.id, req.data);
                        buffer.clear();
                        match err {
                            Ok(()) => Response::MailWritten.encode(&mut buffer),
                            Err(e) => Response::MailWriteFailed(e).encode(&mut buffer),
                        };
                        stream.inner.write(&mut buffer);
                        kad::Record {
                            key: req.id.to_vec().into(),
                            value: PutRecord::Mail(req.data).to_bytes(),
                            publisher: None,
                            expires: None,
                        }
                    }
                };

                // the put method on the store is no op
                swarm
                    .behaviour_mut()
                    .kad
                    .put_record(record, Quorum::N(protocols::chat::REPLICATION_FACTOR))
                    .unwrap();
                continue;
            }
        };

        match e {
            SwarmEvent::Behaviour(BehaviourEvent::Identfy(libp2p::identify::Event::Received {
                peer_id,
                info,
            })) => {
                for addr in info.listen_addrs {
                    swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest { to })) => {
                component_utils::handle_conn_request(to, &mut swarm, &mut peer_discovery)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                if component_utils::try_handle_conn_response(
                    &e,
                    &mut swarm,
                    &mut peer_discovery,
                ) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result: kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk { peers, .. })),
                step,
                ..
            })) if search_queries.iter().any(|&(.., q)| q == id) => {
                let index = search_queries.iter().position(|&(.., q)| q == id).unwrap();
                let (i, chat, mut members, _) = search_queries.remove(index).unwrap();
                members.extend(peers);

                if !step.last {
                    search_queries.push_back((i, chat, members, id));
                    continue;
                }

                buffer.clear();
                Response::SearchResults(SearchResult { members, chat }).encode(&mut buffer);
                let Some(c) = clients.iter_mut().find(|c| c.id == i) else {
                    continue;
                };

                c.inner.write(&mut buffer);
            }
            SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::InboundStream(s))) => {
                log::debug!("inbound stream created from {client_ids}");
                clients.push(Stream {
                    id: client_ids,
                    subscribed: Default::default(),
                    inner: s,
                });
                client_ids += 1;
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::InboundRequest {
                request:
                    InboundRequest::PutRecord {
                        record: Some(record),
                        ..
                    },
            })) => {
                let Some(msg) = PutRecord::decode(&mut record.value.as_slice()) else {
                    continue;
                };

                match msg {
                    PutRecord::Message(msg) => {
                        let Some(chat_name) = std::str::from_utf8(record.key.as_ref())
                            .ok()
                            .map(ChatName::try_from)
                            .and_then(Result::ok)
                        else {
                            continue;
                        };

                        if let Err(e) = swarm
                            .behaviour_mut()
                            .kad
                            .store_mut()
                            .put_message(chat_name, msg)
                        {
                            log::error!("failed to put message: {e:?}");
                            continue;
                        }

                        for client in clients
                            .iter_mut()
                            .filter(|c| c.subscribed.contains(&chat_name))
                        {
                            buffer.clear();
                            Response::Message(Message {
                                chat: chat_name,
                                content: msg.content,
                                content_sig: msg.content_sig,
                                sender: msg.sender,
                            })
                            .encode(&mut buffer);
                            client.inner.write(&mut buffer);
                        }
                    }
                    PutRecord::Mail(msg) => {
                        let Ok(mail_id) = UserMailId::try_from(record.key.as_ref()) else {
                            continue;
                        };

                        if let Err(e) = swarm.behaviour_mut().kad.store_mut().put_mail(mail_id, msg)
                        {
                            log::error!("failed to put notification: {e:?}");
                            continue;
                        }
                    }
                    PutRecord::ChatHistory(_) => todo!(),
                };
            }
            e => {
                log::debug!("{e:?}");
            }
        };
    }
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
    last_action_no: u32,
    data: Vec<u8>,
}

struct ChatStore {
    chats: HashMap<ChatName, ChatState>,
    mail_boxes: HashMap<UserMailId, Profie>,
    mem: MemoryStore,
}

impl ChatStore {
    fn put_mail(&mut self, id: UserMailId, msg: &[u8]) -> Result<(), PutMailError> {
        if msg.len() > protocols::chat::MAX_MAIL_SIZE {
            return Err(PutMailError::MailTooBig);
        }

        let mail = self.mail_boxes.entry(id).or_default();

        if !mail.messages.push(msg.iter().copied()) {
            return Err(PutMailError::MailboxFull);
        }

        Ok(())
    }

    fn read_mail(&mut self, req: MailActionProof) -> Result<&mut [u8], ReadMailError> {
        let mail = self.mail_boxes.entry(req.pk).or_default();

        if !req.is_valid() {
            return Err(ReadMailError::InvalidProof);
        }

        if mail.last_action_no >= req.no {
            return Err(ReadMailError::NotPermitted);
        }

        mail.last_action_no = req.no;
        Ok(mail.messages.read())
    }

    fn read_data(&mut self, id: UserMailId) -> &[u8] {
        self.mail_boxes.get(&id).map_or(&[], |pr| &pr.data)
    }

    fn write_data(&mut self, req: WriteData) -> Result<(), WriteDataError> {
        let mail = self.mail_boxes.entry(req.proof.pk).or_default();

        if !req.proof.is_valid() {
            return Err(WriteDataError::InvalidProof);
        }

        if mail.last_action_no >= req.proof.no {
            return Err(WriteDataError::NotPermitted);
        }

        mail.data.clear();
        mail.data.extend(req.data);

        Ok(())
    }

    fn put_message(&mut self, chat: ChatName, rm: ReplicateMessage) -> Result<(), PutMessageError> {
        let Some(msg) = PrefixedMessage::decode(&mut &*rm.content) else {
            return Err(PutMessageError::InvalidContent);
        };

        if !rm.is_valid() {
            return Err(PutMessageError::InvalidMessage);
        }

        let chat = self.chats.entry(chat).or_default();
        if chat.members.is_empty() {
            chat.members.push(Member {
                id: 0,
                identity: rm.sender,
                perm: 0,
                last_message_no: 0,
            });
            chat.messages
                .push(Base128Bytes::new(0).chain(rm.content.iter().copied()));
            return Ok(());
        }

        let Some(member) = chat.members.iter_mut().find(|m| m.identity == rm.sender) else {
            return Err(PutMessageError::NotMember);
        };

        if member.last_message_no >= msg.no {
            return Err(PutMessageError::MessageNumberTooLow);
        }

        member.last_message_no = msg.no;
        let Some(msg) = MessageContent::decode(&mut &*msg.content) else {
            return Err(PutMessageError::InvalidContent);
        };

        match msg {
            MessageContent::Arbitrary(content)
                if content.len() > protocols::chat::MAX_MESSAGE_SIZE =>
            {
                return Err(PutMessageError::MessageTooBig);
            }
            MessageContent::Arbitrary(content) => chat
                .messages
                .push(Base128Bytes::new(member.id as _).chain(content.iter().copied())),
            MessageContent::AddMember(am) => {
                let issuer_perm = member.perm;
                let Some(member) = chat.members.iter_mut().find(|m| m.identity == am.invited)
                else {
                    let id = chat.members.len() as _;
                    chat.members.push(Member {
                        id,
                        identity: am.invited,
                        perm: issuer_perm + am.perm_offset,
                        last_message_no: 0,
                    });
                    return Ok(());
                };

                if member.perm <= issuer_perm {
                    return Err(PutMessageError::NotPermitted);
                }

                member.perm += issuer_perm + am.perm_offset;
            }
            MessageContent::RemoveMember(id) => {
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
    subscribed: HashSet<ChatName>,
    inner: EncryptedStream,
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
