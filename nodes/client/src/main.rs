#![allow(non_snake_case)]
#![feature(mem_copy_fn)]
use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::task::Poll;
use std::time::Duration;
use std::{io, iter, mem};

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
use libp2p::swarm::{ConnectionHandler, NetworkBehaviour};
use onion::EncryptedStream;
use protocols::chat::{
    chat_name_to_string, string_to_chat_name, FetchMessages, Message, Request, Response,
};
use std::rc::Rc;

pub type ChatName = Rc<str>;
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

    let selected_chat = create_rw_signal(ChatName::from(""));
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
                    name: chat.clone(),
                    messages: VecDeque::new(),
                });
            });
            wcommands(NodeCommand::FetchMessages(chat));
        }
        NodeEvent::Saturated => {}
        NodeEvent::None => {}
    });

    let selected_thread =
        move || threads.with(|t| t.iter().find(|t| t.name == selected_chat()).cloned());

    let thread_buttons =
        move || threads.with(|t| t.iter().map(|t| t.name.clone()).collect::<Vec<_>>());
    let thread_button_view = move |chat: ChatName| {
        let [schat, ochat] = [chat.clone(), chat.clone()];
        let selected = move || selected_chat() == schat;
        let onclick = move |_| selected_chat.set(ochat.clone());
        view! { <button class:selected=selected on:click=onclick>{chat.to_string()}</button> }
    };

    let create_chat = create_node_ref::<Input>();
    let on_create = move |_| {
        let chat = create_chat.get().unwrap().value();
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
        let chat = selected_chat();
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
        chat: String,
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

async fn boot_node(events: WriteSignal<NodeEvent>, commands: ReadSignal<NodeCommand>) {
    use libp2p::core::multiaddr::Protocol;
    use libp2p::*;

    let enough_onion_nodes = 5;
    let bootstrap_node: SocketAddr = (Ipv4Addr::LOCALHOST, 8900).into();

    let keypair = identity::Keypair::generate_ed25519();
    let transport = websocket_websys::Transport::new(100)
        .upgrade(Version::V1Lazy)
        .authenticate(noise::Config::new(&keypair).unwrap())
        .multiplex(yamux::Config::default())
        .boxed();
    let peer_id = keypair.public().to_peer_id();
    let secret = onion::Secret::random();

    let behaviour = Behaviour {
        onion: onion::Behaviour::new(
            onion::Config::new(secret, peer_id).keep_alive_interval(Duration::from_secs(100)),
        ),
        key_share: onion::get_key::Behaviour::new(),
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
    let mut onion_peers = HashMap::new();
    let mut peer_search_route = None::<EncryptedStream>;
    let mut search_route = None;
    let mut buffer = vec![];
    let mut pending_streams = vec![];
    let mut subscriptions = SelectAll::<Subscription>::new();
    let mut connected = false;
    loop {
        enum Event {
            Command(NodeCommand),
            Event(SwarmEvent),
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
                        let chat = chat_name_to_string(&msg.chat).unwrap().into();
                        let raw_msg = RawMessage::decode(&mut &*msg.content).unwrap();
                        events(NodeEvent::NewMessage {
                            chat,
                            name: raw_msg.sender.to_owned(),
                            content: raw_msg.content.to_owned(),
                        });
                        log::info!("new message");
                    }
                    Response::FetchedMessages(fmsg) => {
                        let chat = chat_name_to_string(&fmsg.chat).unwrap().into();
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
                            .cursor = fmsg.cursor;
                        events(NodeEvent::FetchedMessages { chat, messages });
                    }
                    Response::Subscribed(chn) => {
                        events(NodeEvent::Subscribed(
                            chat_name_to_string(&chn).unwrap().into(),
                        ));
                    }
                    Response::SearchResults(_) => log::error!("unepxected response"),
                    Response::NotFound(()) => log::error!("something was not found"),
                }
            }
            Event::SearchPacket(r) => {
                let msg = match r {
                    Ok(m) => m,
                    Err(e) => {
                        log::error!("search path error: {e}");
                        let route: [_; 3] = onion_peers
                            .iter()
                            .map(|(a, b)| (*b, *a))
                            .take(3)
                            .collect::<Vec<_>>()
                            .try_into()
                            .unwrap();

                        search_route = Some(swarm.behaviour_mut().onion.open_path(route));
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
                    .find_map(|p| Some((p, *onion_peers.get(&p)?)))
                else {
                    log::error!("no member peer found");
                    continue;
                };

                log::info!(
                    "peer picked: {} for chat: {}",
                    peer,
                    chat_name_to_string(&res.chat).unwrap()
                );

                let route: [_; 3] = onion_peers
                    .iter()
                    .filter(|&(&a, _)| a != peer)
                    .map(|(a, b)| (*b, *a))
                    .take(2)
                    .chain(iter::once((key, peer)))
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap();

                pending_streams.push((
                    ChatName::from(chat_name_to_string(&res.chat).unwrap()),
                    swarm.behaviour_mut().onion.open_path(route),
                ));

                log::info!("search results");
            }
            Event::Command(c) => match c {
                _ if peer_search_route.is_none() => log::error!("sarch route not present"),
                NodeCommand::Subscrbe { chat } => {
                    buffer.clear();
                    protocols::chat::Request::SearchFor(
                        protocols::chat::string_to_chat_name(&chat).unwrap(),
                    )
                    .encode(&mut buffer);
                    peer_search_route.as_mut().unwrap().write(&mut buffer);
                }
                NodeCommand::SendMessage {
                    chat,
                    name,
                    content,
                } => {
                    let Some(chat) = subscriptions.iter_mut().find(|s| s.chat == chat) else {
                        log::error!("chat not found");
                        continue;
                    };

                    let chat_name = string_to_chat_name(&chat.chat).unwrap();

                    let raw = RawMessage {
                        sender: name.as_str(),
                        content: content.as_str(),
                    }
                    .to_bytes();

                    buffer.clear();
                    Request::Send(Message {
                        chat: chat_name,
                        content: &raw,
                    })
                    .encode(&mut buffer);
                    chat.stream.write(&mut buffer);
                    log::info!("sent message");
                }
                NodeCommand::FetchMessages(chat) => {
                    let chat_name = string_to_chat_name(&chat).unwrap();
                    let chat = subscriptions.iter_mut().find(|s| s.chat == chat).unwrap();

                    buffer.clear();
                    Request::FetchMessages(FetchMessages {
                        chat: chat_name,
                        cursor: chat.cursor,
                    })
                    .encode(&mut buffer);
                    chat.stream.write(&mut buffer);
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
                SwarmEvent::Behaviour(BehaviourEvent::KeyShare((key, peer))) => {
                    log::info!("key share from {}", peer);
                    onion_peers.insert(peer, key);
                    if onion_peers.len() == enough_onion_nodes && search_route.is_none() {
                        let route: [_; 3] = onion_peers
                            .iter()
                            .map(|(a, b)| (*b, *a))
                            .take(3)
                            .collect::<Vec<_>>()
                            .try_into()
                            .unwrap();
                        search_route = Some(swarm.behaviour_mut().onion.open_path(route));
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Onion(onion::Event::OutboundStream(
                    stream,
                    id,
                ))) if Some(id) == search_route => {
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
                    Request::Subscribe(protocols::chat::string_to_chat_name(&chat).unwrap())
                        .encode(&mut buffer);
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
    key_share: onion::get_key::Behaviour,
    kad: libp2p::kad::Behaviour<MemoryStore>,
}

component_utils::impl_kad_search!(Behaviour => (onion::Behaviour => onion));

type SwarmEvent = libp2p::swarm::SwarmEvent<
    BehaviourEvent,
    <<Behaviour as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::Error,
>;
