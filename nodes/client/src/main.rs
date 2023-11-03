#![allow(non_snake_case)]
#![feature(mem_copy_fn)]
use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::task::Poll;
use std::time::Duration;
use std::{io, iter, mem};

use chain_api::NodeData;
use component_utils::futures::stream::SelectAll;
use component_utils::futures::FutureExt;
use component_utils::kad::KadPeerSearch;
use component_utils::Codec;
use leptos::html::Input;
use leptos::signal_prelude::*;
use leptos::*;
use libp2p::core::upgrade::Version;
use libp2p::core::ConnectedPoint;
use libp2p::futures::StreamExt;
use libp2p::kad::store::MemoryStore;
use libp2p::swarm::SwarmEvent;
use libp2p::PeerId;
use onion::EncryptedStream;
use protocols::chat::{ChatName, FetchMessages, FetchedMessages, Message, Request, Response};
use std::rc::Rc;

pub type Sender = Rc<str>;
pub type Content = Rc<str>;

component_utils::protocol! { 'a:
    #[derive(Clone, Copy)]
    struct RawMessage<'a> {
        sender: &'a str,
        content: &'a str,
    }
}

pub fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(log::Level::Info);
    mount_to_body(App)
}

fn App() -> impl IntoView {
    let (revents, wevents) = create_signal(NodeEvent::None);
    let (rcommands, wcommands) = create_signal(NodeCommand::None);
    spawn_local(boot_node(wevents, rcommands));

    let saturated = create_memo(move |prev| {
        prev.map_or(false, mem::copy) | matches!(revents(), NodeEvent::Saturated)
    });

    let body = move || {
        if saturated() {
            view! { <Chat revents wcommands/> }.into_view()
        } else {
            view! { <Saturation/> }.into_view()
        }
    };

    view! { <div>{body}</div> }
}

fn Saturation() -> impl IntoView {
    "Initiating onion rings"
}

#[derive(Clone)]
struct ThreadMessage {
    name: Sender,
    content: Content,
    id: usize,
}

#[leptos::component]
fn Chat(revents: ReadSignal<NodeEvent>, wcommands: WriteSignal<NodeCommand>) -> impl IntoView {
    #[derive(Clone)]
    struct Thread {
        name: ChatName,
        messages: VecDeque<ThreadMessage>,
    }

    let selected_chat = create_rw_signal(None::<ChatName>);
    let threads = create_rw_signal(Vec::<Thread>::new());
    create_effect(move |_| match revents() {
        NodeEvent::NewMessage {
            chat,
            name,
            content,
        } => {
            threads.update(|t| {
                let Some(thread) = t.iter_mut().find(|t| t.name == chat) else {
                    log::error!("chat not found for the new message");
                    return;
                };
                let len = thread.messages.len();
                thread.messages.push_front(ThreadMessage {
                    name: name.into(),
                    content: content.into(),
                    id: len,
                });
            });
        }
        NodeEvent::FetchedMessages { chat, mut messages } => {
            threads.update(|t| {
                let Some(thread) = t.iter_mut().find(|t| t.name == chat) else {
                    log::error!("chat not found for the new message");
                    return;
                };
                messages
                    .iter_mut()
                    .zip(thread.messages.len()..)
                    .for_each(|(m, i)| m.id = i);
                thread.messages.extend(messages.into_iter());
            });
        }
        NodeEvent::Subscribed(chat) => {
            log::info!("subscribed to {}", chat);
            threads.update(|t| {
                t.push(Thread {
                    name: chat,
                    messages: VecDeque::new(),
                });
            });
            wcommands(NodeCommand::FetchMessages(chat));
        }
        NodeEvent::Saturated => {}
        NodeEvent::None => {}
    });

    let selected_thread =
        move || threads.with(|t| t.iter().find(|t| Some(t.name) == selected_chat()).cloned());

    let thread_buttons = move || threads.with(|t| t.iter().map(|t| t.name).collect::<Vec<_>>());
    let thread_button_view = move |chat: ChatName| {
        let selected = move || selected_chat() == Some(chat);
        let onclick = move |_| selected_chat.set(Some(chat));
        view! { <button class:selected=selected on:click=onclick>{chat.as_string()}</button> }
    };

    let create_chat = create_node_ref::<Input>();
    let on_create = move |_| {
        let chat = create_chat.get().unwrap().value();
        let chat = ChatName::new(chat.as_str()).unwrap();
        wcommands(NodeCommand::Subscrbe { chat });
    };

    let thread_messages = move || {
        selected_thread()
            .map_or([].into(), |t| t.messages.clone())
            .into_iter()
            .rev()
    };
    let thread_message_view = move |msg: ThreadMessage| {
        view! {
            <div>
                <div>{msg.name.to_string()}</div>
                <div>{msg.content.to_string()}</div>
            </div>
        }
    };

    let message_input = create_node_ref::<Input>();
    let on_send = move |_| {
        let name = "anon";
        let content = message_input.get().unwrap().value();
        let Some(chat) = selected_chat() else {
            log::error!("no chat selected");
            return;
        };
        wcommands(NodeCommand::SendMessage {
            chat,
            name: name.into(),
            content,
        });
        message_input.get().unwrap().set_value("");
    };

    view! {
        <div>
            <For each=thread_buttons key=Clone::clone children=thread_button_view/>
            <input type="text" placeholder="chat" _ref=create_chat/>
            <button on:click=on_create>Create</button>
        </div>
        <div>
            <For each=thread_messages key=|m| m.id children=thread_message_view/>
        </div>
        <div>
            <input type="text" placeholder="msg" _ref=message_input/>
            <button on:click=on_send>Send</button>
        </div>
    }
}

#[derive(Clone)]
enum NodeEvent {
    NewMessage {
        chat: ChatName,
        name: String,
        content: String,
    },
    FetchedMessages {
        chat: ChatName,
        messages: Vec<ThreadMessage>,
    },
    Subscribed(ChatName),
    Saturated,
    None,
}

#[derive(Clone)]
enum NodeCommand {
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

fn node_data_to_path_seg(data: NodeData) -> (PeerId, onion::PublicKey) {
    let key = data.enc;
    let peer_id = component_utils::libp2p_identity::PublicKey::from(
        component_utils::libp2p_identity::ed25519::PublicKey::try_from_bytes(
            &crypto::sign::PublicKey::from(data.sign).ed,
        )
        .unwrap(),
    )
    .to_peer_id();
    (peer_id, key.into())
}

async fn boot_node(events: WriteSignal<NodeEvent>, commands: ReadSignal<NodeCommand>) {
    use libp2p::core::multiaddr::Protocol;
    use libp2p::*;

    let bootstrap_node: SocketAddr = (Ipv4Addr::LOCALHOST, 8900).into();

    let nodes = chain_api::nodes("http://localhost:8700")
        .await
        .unwrap()
        .into_iter()
        .map(node_data_to_path_seg)
        .collect::<HashMap<_, _>>();

    let keys = protocols::chat::UserKeys::new();
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
                kad::Config::default().set_replication_factor(protocols::chat::REPLICATION_FACTOR),
            ),
        ),
    };

    let mut swarm = swarm::Swarm::new(
        transport,
        behaviour,
        peer_id,
        libp2p::swarm::Config::with_wasm_executor(),
    );

    swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Client));

    // rely on a fact that hash map has random order of iteration (RandomState)
    let route: [_; 3] = nodes
        .iter()
        .map(|(a, b)| (*b, *a))
        .take(3)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    let mut commands = commands.to_stream().fuse();

    fn dile_addr(addr: SocketAddr) -> Multiaddr {
        Multiaddr::empty()
            .with(match addr {
                SocketAddr::V4(addr) => Protocol::Ip4(addr.ip().to_owned()),
                SocketAddr::V6(addr) => Protocol::Ip6(addr.ip().to_owned()),
            })
            .with(Protocol::Tcp(addr.port()))
            .with(Protocol::Ws("/".into()))
    }

    swarm.dial(dile_addr(bootstrap_node)).unwrap();

    let mut discovery = KadPeerSearch::default();
    let mut peer_search_route = None::<EncryptedStream>;
    let mut search_route = swarm.behaviour_mut().onion.open_path(route).unwrap();
    let mut buffer = vec![];
    let mut pending_streams = vec![];
    let mut subscriptions = SelectAll::<Subscription>::new();
    let mut connected = false;
    loop {
        enum Event<A, B> {
            Command(NodeCommand),
            Event(SwarmEvent<A, B>),
            SearchPacket(io::Result<Vec<u8>>),
            SubscriptionPacket(io::Result<Vec<u8>>),
        }

        let search_future = std::future::poll_fn(|cx| match peer_search_route.as_mut() {
            Some(s) => s.poll(cx).map_ok(|v| v.to_vec()),
            None => Poll::Pending,
        });

        let e = futures::select! {
            command = commands.select_next_some() => Event::Command(command),
            event = swarm.select_next_some() => Event::Event(event),
            packet = search_future.fuse() => Event::SearchPacket(packet),
            packet = subscriptions.select_next_some() => Event::SubscriptionPacket(packet),
        };

        match e {
            Event::SubscriptionPacket(r) => {
                let msg = match r {
                    Ok(m) => m,
                    Err(e) => {
                        log::error!("chat subscription error: {e}");
                        continue;
                    }
                };

                let Some(pckt) = Response::decode(&mut msg.as_slice()) else {
                    continue;
                };

                match pckt {
                    Response::Message(msg) => {
                        let raw_msg = RawMessage::decode(&mut &*msg.content).unwrap();
                        events(NodeEvent::NewMessage {
                            chat: msg.chat,
                            name: raw_msg.sender.to_owned(),
                            content: raw_msg.content.to_owned(),
                        });
                        log::info!("new message");
                    }
                    Response::FetchedMessages(fmsg @ FetchedMessages { chat, cursor, .. }) => {
                        let messages = fmsg
                            .messages()
                            .map(|r| RawMessage::decode(&mut &*r).unwrap())
                            .map(|r| ThreadMessage {
                                name: r.sender.into(),
                                content: r.content.into(),
                                id: 0,
                            })
                            .collect::<Vec<_>>();
                        subscriptions
                            .iter_mut()
                            .find(|s| s.chat == chat)
                            .unwrap()
                            .cursor = cursor;
                        events(NodeEvent::FetchedMessages { chat, messages });
                    }
                    Response::Subscribed(chn) => {
                        events(NodeEvent::Subscribed(chn));
                    }
                    Response::SearchResults(_) => log::error!("unepxected response"),
                    Response::ChatNotFound => log::error!("chat not found"),
                    Response::FailedMessage(e) => log::error!("failed to put message: {:?}", e),
                }
            }
            Event::SearchPacket(r) => {
                let msg = match r {
                    Ok(m) => m,
                    Err(e) => {
                        log::error!("search path error: {e}");
                        let route: [_; 3] = nodes
                            .iter()
                            .map(|(a, b)| (*b, *a))
                            .take(3)
                            .collect::<Vec<_>>()
                            .try_into()
                            .unwrap();

                        search_route = swarm.behaviour_mut().onion.open_path(route).unwrap();
                        peer_search_route = None;
                        continue;
                    }
                };

                let Some(Response::SearchResults(res)) = Response::decode(&mut &*msg) else {
                    log::error!("search packet is malformed");
                    continue;
                };

                let Some((peer, key)) = res
                    .members
                    .into_iter()
                    .skip(*peer_id.to_bytes().last().unwrap() as usize % 2)
                    .find_map(|p| Some((p, *nodes.get(&p)?)))
                else {
                    log::error!("no member peer found");
                    continue;
                };

                log::info!("peer picked: {} for chat: {}", peer, res.chat);

                let route: [_; 3] = nodes
                    .iter()
                    .filter(|&(&a, _)| a != peer)
                    .map(|(a, b)| (*b, *a))
                    .take(2)
                    .chain(iter::once((key, peer)))
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap();

                pending_streams.push((
                    res.chat,
                    swarm.behaviour_mut().onion.open_path(route).unwrap(),
                ));

                log::info!("search results");
            }
            Event::Command(c) => match c {
                _ if peer_search_route.is_none() => log::error!("sarch route not present"),
                NodeCommand::Subscrbe { chat } => {
                    buffer.clear();
                    protocols::chat::Request::SearchFor(chat).encode(&mut buffer);
                    peer_search_route.as_mut().unwrap().write(&mut buffer);
                }
                NodeCommand::SendMessage {
                    chat,
                    name,
                    content,
                } => {
                    let Some(sub) = subscriptions.iter_mut().find(|s| s.chat == chat) else {
                        log::error!("chat not found");
                        continue;
                    };

                    let raw = RawMessage {
                        sender: name.as_str(),
                        content: content.as_str(),
                    }
                    .to_bytes();

                    buffer.clear();
                    Request::Send(Message {
                        chat,
                        content: &raw,
                        content_sig: keys.sign.sign(&raw).into(),
                        sender: keys.identity().sign.into(),
                    })
                    .encode(&mut buffer);
                    sub.stream.write(&mut buffer);
                    log::info!("sent message");
                }
                NodeCommand::FetchMessages(chat) => {
                    let sub = subscriptions.iter_mut().find(|s| s.chat == chat).unwrap();

                    buffer.clear();
                    Request::FetchMessages(FetchMessages {
                        chat,
                        cursor: sub.cursor,
                    })
                    .encode(&mut buffer);
                    sub.stream.write(&mut buffer);
                }
                NodeCommand::None => {}
            },
            Event::Event(event) => match event {
                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint: ConnectedPoint::Dialer { address, .. },
                    ..
                } => {
                    swarm.behaviour_mut().kad.add_address(&peer_id, address);
                    if !mem::replace(&mut connected, true) {
                        swarm.behaviour_mut().kad.bootstrap().unwrap();
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::ConnectRequest {
                    to,
                })) => {
                    component_utils::handle_conn_request(to, &mut swarm, &mut discovery);
                }
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                ))) if id == search_route => {
                    peer_search_route = Some(stream);
                    events(NodeEvent::Saturated);
                }
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    mut stream,
                    id,
                ))) if pending_streams.iter().any(|&(_, pid)| id == pid) => {
                    let index = pending_streams
                        .iter()
                        .position(|&(_, pid)| id == pid)
                        .unwrap();
                    let (chat, _) = pending_streams.swap_remove(index);
                    buffer.clear();
                    Request::Subscribe(chat).encode(&mut buffer);
                    log::debug!("subscribing to {}", chat);
                    stream.write(&mut buffer);
                    subscriptions.push(Subscription {
                        chat,
                        stream,
                        cursor: protocols::chat::NO_CURSOR,
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kad(e))
                    if component_utils::try_handle_conn_response(
                        &e,
                        &mut swarm,
                        &mut discovery,
                    ) => {}
                SwarmEvent::Behaviour(BehaviourEvent::Onion(o)) => {
                    panic!("unexpected onion event: {:?}", o)
                }
                e => logging::log!("{:?}", e),
            },
        }
    }
}

struct Subscription {
    chat: ChatName,
    stream: EncryptedStream,
    cursor: protocols::chat::Cursor,
}

impl component_utils::futures::Stream for Subscription {
    type Item = <EncryptedStream as component_utils::futures::Stream>::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.stream.poll_next_unpin(cx)
    }
}

#[derive(libp2p::swarm::NetworkBehaviour)]
struct Behaviour {
    onion: onion::Behaviour,
    kad: libp2p::kad::Behaviour<MemoryStore>,
}

component_utils::impl_kad_search!(Behaviour => (onion::Behaviour => onion));
