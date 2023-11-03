use std::{pin::Pin, time::Duration};

use futures::{FutureExt, StreamExt};
use libp2p_core::{multiaddr::Protocol, upgrade::Version, Transport};
use libp2p_identity::{Keypair, PeerId};
use libp2p_swarm::SwarmEvent;

use crate::EncryptedStream;

const CONNECTION_TIMEOUT: Duration = Duration::from_millis(1000);

fn setup_nodes<const COUNT: usize>(
    ports: [u16; COUNT],
) -> [libp2p_swarm::Swarm<crate::Behaviour>; COUNT] {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(env_logger::init);
    ports.map(|port| {
        let keypair = Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let secret = crate::KeyPair::default();
        let transport = libp2p::tcp::tokio::Transport::default()
            .upgrade(Version::V1)
            .authenticate(libp2p::noise::Config::new(&keypair).unwrap())
            .multiplex(libp2p::yamux::Config::default())
            .boxed();
        let mut swarm = libp2p_swarm::Swarm::new(
            transport,
            crate::Behaviour::new(
                crate::Config::new(Some(secret), peer_id).keep_alive_interval(CONNECTION_TIMEOUT),
            ),
            peer_id,
            libp2p_swarm::Config::with_tokio_executor()
                .with_idle_connection_timeout(CONNECTION_TIMEOUT),
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
    swarms: &mut [libp2p_swarm::Swarm<crate::Behaviour>],
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
            SwarmEvent::Behaviour(crate::Event::InboundStream(s)) => input = Some(s),
            SwarmEvent::Behaviour(crate::Event::OutboundStream(s, _)) => output = Some(s),
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
    let (mut input, mut output) = open_path(&mut swarms).await;

    swarms.reverse();
    swarms
        .array_chunks_mut()
        .for_each(|[a, b]| std::mem::swap(a, b));

    open_path(&mut swarms).await;

    input.write(&mut b"hello".to_vec());

    let mut disconnected = 0;
    let mut timeout = Box::pin(tokio::time::sleep(CONNECTION_TIMEOUT * 10));

    while disconnected != 6 {
        let events = futures::future::select_all(swarms.iter_mut().map(|s| s.next()));
        let e = futures::select! {
            (e, ..) = events.fuse() => e,
            _ = Pin::new(&mut timeout).fuse() => panic!("{disconnected} nodes disconnected"),
            r = output.select_next_some() => {
                let mut msg = r.unwrap();
                input.write(&mut msg);
                continue;
            },
        };

        match e.unwrap() {
            SwarmEvent::ConnectionClosed { .. } => disconnected += 1,
            e => log::debug!("{e:?}"),
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
                SwarmEvent::Behaviour(crate::Event::ConnectRequest { to }) => {
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
