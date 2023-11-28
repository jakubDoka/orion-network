use {
    crate::{EncryptedStream, PathId},
    component_utils::{impl_kad_search, AsocStream, KadPeerSearch},
    futures::{stream::SelectAll, FutureExt, StreamExt},
    libp2p::{
        core::{multiaddr::Protocol, upgrade::Version, Transport},
        identity::{Keypair, PeerId},
        kad::{store::MemoryStore, Mode},
        swarm::{NetworkBehaviour, SwarmEvent},
    },
    rand::seq::SliceRandom,
    std::{collections::HashSet, io, mem, num::NonZeroUsize, pin::Pin, time::Duration, usize},
};

const CONNECTION_TIMEOUT: Duration = Duration::from_millis(1000);

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(env_logger::init);
}

fn setup_nodes<const COUNT: usize>(
    ports: [u16; COUNT],
) -> [libp2p::swarm::Swarm<crate::Behaviour>; COUNT] {
    init();
    ports.map(|port| {
        let keypair = Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let secret = crate::KeyPair::default();
        let transport = libp2p::tcp::tokio::Transport::default()
            .upgrade(Version::V1)
            .authenticate(libp2p::noise::Config::new(&keypair).unwrap())
            .multiplex(libp2p::yamux::Config::default())
            .boxed();
        let mut swarm = libp2p::swarm::Swarm::new(
            transport,
            crate::Behaviour::new(
                crate::Config::new(Some(secret), peer_id).keep_alive_interval(CONNECTION_TIMEOUT),
            ),
            peer_id,
            libp2p::swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(CONNECTION_TIMEOUT * 5),
        );
        swarm.add_external_address(
            libp2p::core::Multiaddr::empty()
                .with(Protocol::Ip4([0, 0, 0, 0].into()))
                .with(Protocol::Tcp(port)),
        );

        swarm
            .listen_on(
                libp2p::core::Multiaddr::empty()
                    .with(Protocol::Ip4([0, 0, 0, 0].into()))
                    .with(Protocol::Tcp(port)),
            )
            .unwrap();

        ports.map(|other| {
            if other >= port {
                return;
            }

            swarm
                .dial(
                    libp2p::core::Multiaddr::empty()
                        .with(Protocol::Ip4([127, 0, 0, 1].into()))
                        .with(Protocol::Tcp(other)),
                )
                .unwrap();
        });

        swarm
    })
}

async fn open_path(
    swarms: &mut [libp2p::swarm::Swarm<crate::Behaviour>],
) -> (EncryptedStream, EncryptedStream) {
    let Ok([_, path @ ..]): Result<[_; 4], _> = swarms
        .iter()
        .map(|s| {
            (
                s.behaviour().config().secret.clone(),
                s.behaviour().config().current_peer_id,
            )
        })
        .collect::<Vec<_>>()
        .try_into()
    else {
        panic!("failed to create path")
    };

    swarms[0]
        .behaviour_mut()
        .open_path(path.map(|(k, i)| (k.unwrap().public_key(), i)))
        .unwrap();

    let mut input = None;
    let mut output = None;
    loop {
        let (e, id, ..) = futures::future::select_all(swarms.iter_mut().map(|s| s.next())).await;
        match e.unwrap() {
            SwarmEvent::Behaviour(crate::Event::InboundStream(s, ..)) => input = Some(s),
            SwarmEvent::Behaviour(crate::Event::OutboundStream(s, ..)) => output = Some(s),
            e => log::debug!("{id} {e:?}"),
        }

        if input.is_some() && output.is_some() {
            let (Some(input), Some(output)) = (input, output) else {
                unreachable!();
            };
            break (input, output);
        }
    }
}

#[tokio::test]
async fn test_routing() {
    let mut swarms = setup_nodes([8800, 8801, 8802, 8803]);
    let (mut input, mut output) = open_path(&mut swarms).await;

    input.write(&mut b"hello".to_vec());
    let r = loop {
        let events = futures::future::select_all(swarms.iter_mut().map(|s| s.next()));
        let e = futures::select! {
            (e, ..) = events.fuse() => e,
            _ = input.select_next_some() => continue,
            r = output.select_next_some() => break r,
        };
        log::debug!("{:?}", e.unwrap());
    };

    assert_eq!(&r.unwrap(), b"hello");
}

#[tokio::test]
async fn test_timeout() {
    let mut swarms = setup_nodes([8804, 8805, 8806, 8807]);

    swarms.reverse();
    swarms
        .array_chunks_mut()
        .for_each(|[a, b]| std::mem::swap(a, b));

    let (mut input, mut output) = open_path(&mut swarms).await;

    input.write(&mut b"hello".to_vec());

    let mut disconnected = 0;
    let mut timeout = Box::pin(tokio::time::sleep(CONNECTION_TIMEOUT * 10));

    while disconnected != 6 {
        let events = futures::future::select_all(swarms.iter_mut().map(|s| s.next()));
        let (e, i) = futures::select! {
            (e, i, ..) = events.fuse() => (e, i),
            _ = Pin::new(&mut timeout).fuse() => panic!("{disconnected} nodes disconnected"),
            r = output.select_next_some() => {
                let mut msg = r.unwrap();
                input.write(&mut msg);
                continue;
            },
        };

        match e.unwrap() {
            SwarmEvent::Behaviour(crate::Event::ConnectRequest(to)) => {
                swarms[i].behaviour_mut().report_unreachable(to);
            }
            SwarmEvent::ConnectionClosed { .. } => disconnected += 1,
            e => log::info!("{e:?}"),
        }
    }
}

#[tokio::test]
async fn test_missing_route() {
    async fn perform(index: usize) {
        let mut swarms = setup_nodes([8808, 8809, 8810, 8811].map(|p| p + index as u16 * 4));
        let Ok([_, mut path @ ..]): Result<[_; 4], _> = swarms
            .iter()
            .map(|s| {
                (
                    s.behaviour().config().secret.clone(),
                    s.behaviour().config().current_peer_id,
                )
            })
            .collect::<Vec<_>>()
            .try_into()
        else {
            panic!("failed to create path")
        };

        path[index].1 = PeerId::random();

        swarms[0]
            .behaviour_mut()
            .open_path(path.map(|(k, i)| (k.unwrap().public_key(), i)))
            .unwrap();

        loop {
            let (e, id, ..) =
                futures::future::select_all(swarms.iter_mut().map(|s| s.next())).await;
            match e.unwrap() {
                SwarmEvent::Behaviour(crate::Event::ConnectRequest(to)) => {
                    swarms[id].behaviour_mut().report_unreachable(to);
                }
                SwarmEvent::Behaviour(crate::Event::Error(crate::Error::MissingPeerLocally)) => {
                    break;
                }
                e => log::debug!("{id} {e:?}"),
            }
        }
    }

    futures::future::join_all((0..3).map(perform)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn settle_down() {
    init();
    let server_count = 10;
    let client_count = 1;
    let max_open_streams_server = 10;
    let concurrent_routes = 1;
    let max_open_streams_client = 1;
    let first_port = 8900;
    let spacing = Duration::from_millis(0);
    let keep_alive_interval = Duration::from_secs(5);

    let replication_factor = NonZeroUsize::new(3).unwrap();
    let (swarms, node_data): (Vec<_>, Vec<_>) = (0..server_count)
        .map(|i: u16| {
            let kp = Keypair::generate_ed25519();
            let peer_id = kp.public().to_peer_id();
            let secret = crate::KeyPair::default();

            let transport = libp2p::tcp::tokio::Transport::default()
                .upgrade(Version::V1)
                .authenticate(libp2p::noise::Config::new(&kp).unwrap())
                .multiplex(libp2p::yamux::Config::default())
                .boxed();

            let beh = SDBehaviour {
                onion: crate::Behaviour::new(
                    crate::Config::new(Some(secret.clone()), peer_id)
                        .keep_alive_interval(keep_alive_interval),
                ),
                kad: libp2p::kad::Behaviour::with_config(
                    peer_id,
                    MemoryStore::new(peer_id),
                    mem::take(
                        libp2p::kad::Config::default().set_replication_factor(replication_factor),
                    ),
                ),
                indentify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                    "l".into(),
                    kp.public(),
                )),
            };

            let mut swarm = libp2p::swarm::Swarm::new(
                transport,
                beh,
                peer_id,
                libp2p::swarm::Config::with_tokio_executor()
                    .with_idle_connection_timeout(keep_alive_interval),
            );

            swarm
                .listen_on(
                    libp2p::core::Multiaddr::empty()
                        .with(Protocol::Ip4([0, 0, 0, 0].into()))
                        .with(Protocol::Tcp(first_port + i)),
                )
                .unwrap();
            if let Some(offset) = i.checked_sub(1) {
                swarm
                    .dial(
                        libp2p::core::Multiaddr::empty()
                            .with(Protocol::Ip4([127, 0, 0, 1].into()))
                            .with(Protocol::Tcp(first_port + offset)),
                    )
                    .unwrap();
            }
            swarm.behaviour_mut().kad.set_mode(Some(Mode::Server));

            (swarm, (peer_id, secret.public_key()))
        })
        .unzip();

    fn handle_packet(
        id: PathId,
        packet: io::Result<Vec<u8>>,
        streams: &mut SelectAll<AsocStream<PathId, EncryptedStream>>,
        counter: &mut usize,
    ) {
        let mut packet = match packet {
            Ok(packet) => packet,
            Err(e) => {
                log::info!("stream terminated with {e:?}");
                return;
            }
        };

        //log::info!("received {packet:?} bytes from {id}");

        *counter += 1;
        if *counter % 10 == 0 {
            log::info!("received {counter} packets");
        }

        let stream = streams.iter_mut().find(|s| s.assoc == id).unwrap();
        stream.inner.write(&mut packet);
    }

    tokio::spawn(async move {
        for mut swarm in swarms {
            tokio::time::sleep(spacing).await;
            tokio::spawn(async move {
                use {
                    crate::Event as OE, libp2p::identify::Event as IE, SDBehaviourEvent as BE,
                    SwarmEvent as SE,
                };

                let mut discovery = KadPeerSearch::default();
                let mut streams = SelectAll::<AsocStream<PathId, EncryptedStream>>::new();
                let mut counter = 0;
                loop {
                    let ev = futures::select! {
                        ev = swarm.select_next_some() => ev,
                        (id, packet) = streams.select_next_some() => { handle_packet(id, packet, &mut streams, &mut counter); continue; },
                    };

                    match ev {
                        SE::Behaviour(BE::Indentify(IE::Received { peer_id, info })) => {
                            if let Some(addr) = info.listen_addrs.first() {
                                swarm
                                    .behaviour_mut()
                                    .kad
                                    .add_address(&peer_id, addr.clone());
                            }
                        }
                        SE::Behaviour(BE::Onion(OE::ConnectRequest(to))) => {
                            component_utils::handle_conn_request(to, &mut swarm, &mut discovery);
                        }
                        SE::Behaviour(BE::Kad(e))
                            if component_utils::try_handle_conn_response(
                                &e,
                                &mut swarm,
                                &mut discovery,
                            ) => {}
                        SE::Behaviour(BE::Onion(OE::InboundStream(stream, pid))) => {
                            if streams.len() > max_open_streams_server {
                                log::info!("too many open streams");
                                continue;
                            }

                            streams.push(AsocStream::new(stream, pid));
                        }
                        e => log::debug!("{e:?}"),
                    }
                }
            });
        }
    });

    let clients = (0..client_count)
        .map(|i| {
            let kp = Keypair::generate_ed25519();
            let peer_id = kp.public().to_peer_id();

            let transport = libp2p::tcp::tokio::Transport::default()
                .upgrade(Version::V1)
                .authenticate(libp2p::noise::Config::new(&kp).unwrap())
                .multiplex(libp2p::yamux::Config::default())
                .boxed();

            let beh = SDBehaviour {
                onion: crate::Behaviour::new(
                    crate::Config::new(None, peer_id).keep_alive_interval(Duration::from_secs(5)),
                ),
                kad: libp2p::kad::Behaviour::with_config(
                    peer_id,
                    MemoryStore::new(peer_id),
                    mem::take(
                        libp2p::kad::Config::default().set_replication_factor(replication_factor),
                    ),
                ),
                indentify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                    "l".into(),
                    kp.public(),
                )),
            };

            let mut swarm = libp2p::swarm::Swarm::new(
                transport,
                beh,
                peer_id,
                libp2p::swarm::Config::with_tokio_executor()
                    .with_idle_connection_timeout(keep_alive_interval),
            );

            swarm.behaviour_mut().kad.set_mode(Some(Mode::Server));

            swarm
                .dial(
                    libp2p::core::Multiaddr::empty()
                        .with(Protocol::Ip4([127, 0, 0, 1].into()))
                        .with(Protocol::Tcp(first_port + i)),
                )
                .unwrap();

            swarm
        })
        .collect::<SelectAll<_>>();

    for mut swarm in clients {
        tokio::time::sleep(spacing).await;
        let node_data = node_data.clone();
        tokio::spawn(async move {
            use {
                crate::Event as OE, libp2p::identify::Event as IE, SDBehaviourEvent as BE,
                SwarmEvent as SE,
            };

            let mut discovery = KadPeerSearch::default();
            let mut streams = SelectAll::<AsocStream<PathId, EncryptedStream>>::new();
            let mut pending_routes = HashSet::new();
            let peer_id = *swarm.local_peer_id();
            let mut connected = false;
            let mut counter = 0;
            loop {
                let ev = futures::select! {
                    ev = swarm.select_next_some() => ev,
                    (id, packet) = streams.select_next_some() => { handle_packet(id, packet, &mut streams, &mut counter); continue; },
                };

                if pending_routes.len() < concurrent_routes
                    && streams.len() < max_open_streams_client
                    && connected
                {
                    let mut rng = &mut rand::thread_rng();
                    let to_dail: [_; 3] = node_data
                        .choose_multiple(&mut rng, node_data.len())
                        .map(|(id, pk)| (*pk, *id))
                        .take(3)
                        .collect::<Vec<_>>()
                        .try_into()
                        .unwrap();
                    let id = swarm.behaviour_mut().onion.open_path(to_dail).unwrap();
                    pending_routes.insert((id, peer_id));
                }

                match ev {
                    SE::Behaviour(BE::Indentify(IE::Received { peer_id, info })) => {
                        connected = true;
                        if let Some(addr) = info.listen_addrs.first() {
                            swarm
                                .behaviour_mut()
                                .kad
                                .add_address(&peer_id, addr.clone());
                        }
                    }
                    SE::Behaviour(BE::Onion(OE::ConnectRequest(to))) => {
                        log::error!("connect request {to:?}");
                        component_utils::handle_conn_request(to, &mut swarm, &mut discovery);
                    }
                    SE::Behaviour(BE::Kad(e))
                        if component_utils::try_handle_conn_response(
                            &e,
                            &mut swarm,
                            &mut discovery,
                        ) =>
                    {
                        log::error!("kad event {e:?}");
                    }
                    SE::Behaviour(BE::Onion(OE::OutboundStream(mut stream, id, ..))) => {
                        stream.write(&mut b"hello".to_vec());
                        streams.push(AsocStream::new(stream, id));
                        pending_routes.remove(&(id, peer_id));
                    }
                    e => log::info!("{e:?}"),
                }
            }
        });
    }

    std::future::pending().await
}

#[derive(NetworkBehaviour)]
struct SDBehaviour {
    onion: crate::Behaviour,
    kad: libp2p::kad::Behaviour<MemoryStore>,
    indentify: libp2p::identify::Behaviour,
}

impl_kad_search!(SDBehaviour => (crate::Behaviour => onion));
