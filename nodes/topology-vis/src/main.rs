use {
    libp2p::{
        core::{upgrade::Version, ConnectedPoint},
        futures::StreamExt,
        kad::ProgressStep,
        swarm::NetworkBehaviour,
        Multiaddr, PeerId, Transport,
    },
    macroquad::prelude::*,
    std::{cell::RefCell, collections::BTreeSet, mem},
    wasm_bindgen_futures::spawn_local,
};

#[derive(Debug, Clone, Copy)]
struct Node {
    pid: Option<PeerId>,
    position: Vec2,
    velocity: Vec2,
}

impl Node {
    const FRICION: f32 = 0.05;
    const LINE_THICKNESS: f32 = 3.0;
    const NODE_SIZE: f32 = 20.0;

    fn new(x: f32, y: f32, pid: PeerId) -> Self {
        Self {
            pid: Some(pid),
            position: Vec2::new(x, y),
            velocity: Vec2::from_angle(rand::gen_range(0.0, 2.0 * std::f32::consts::PI)) * 100.0,
        }
    }

    fn apply_forces(
        &mut self,
        other: &mut Self,
        time: f32,
        balanced_distance: f32,
        dont_atract: bool,
        force_strength: f32,
    ) {
        let diff = other.position - self.position;
        let dist_sq = diff.length_squared();
        let force = dist_sq - balanced_distance * balanced_distance;
        if force > 0.0 && dont_atract {
            return;
        }
        let normalized = diff.try_normalize().unwrap_or(Vec2::new(1.0, 0.0));
        self.velocity += normalized * force * time * force_strength;
        other.velocity -= normalized * force * time * force_strength;
    }

    fn update(&mut self, time: f32) {
        self.position += self.velocity * time;
        self.velocity *= 1.0 - Self::FRICION;
    }

    fn draw(&self) {
        draw_circle(self.position.x, self.position.y, Self::NODE_SIZE, RED);
    }

    fn draw_connection(&self, other: &Self, color: Color, index: usize) {
        draw_line(
            self.position.x,
            self.position.y,
            other.position.x,
            other.position.y,
            Self::LINE_THICKNESS * (index + 1) as f32,
            color,
        );
    }
}

#[derive(Debug)]
struct Protocol {
    name: String,
    color: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    start: usize,
    end: usize,
    protocol: usize,
    connection: usize,
}

#[derive(Default)]
struct World {
    nodes: Vec<Node>,
    free_nodes: Vec<usize>,
    edges: BTreeSet<Edge>,
    protocols: Vec<Protocol>,
}

#[allow(dead_code)]
impl World {
    fn add_node(&mut self, node: Node) -> usize {
        if let Some(index) = self.free_nodes.pop() {
            self.nodes[index] = node;
            return index;
        }

        let index = self.nodes.len();
        self.nodes.push(node);
        index
    }

    fn add_protocol(&mut self, protocol: &str) -> usize {
        if let Some(index) = self.protocols.iter().position(|p| p.name == protocol) {
            return index;
        }

        let index = self.protocols.len();
        self.protocols.push(Protocol {
            name: protocol.to_owned(),
            color: Color::new(
                rand::gen_range(0.5, 1.0),
                rand::gen_range(0.5, 1.0),
                rand::gen_range(0.5, 1.0),
                1.0,
            ),
        });
        index
    }

    fn add_edge(&mut self, edge: Edge) {
        debug_assert!(edge.start != edge.end);
        self.edges.insert(edge);
    }

    fn remove_edge(&mut self, edge: Edge) {
        debug_assert!(edge.start != edge.end);
        self.edges.remove(&edge);
    }

    fn remove_node(&mut self, index: usize) {
        self.nodes[index].pid = None;
        self.free_nodes.push(index);
    }

    fn update(&mut self, time: f32) {
        for edge in &self.edges {
            let mut start = self.nodes[edge.start];
            let mut end = self.nodes[edge.end];
            start.apply_forces(&mut end, time, 100.0, false, 0.2);
            self.nodes[edge.start] = start;
            self.nodes[edge.end] = end;
        }

        let mut iter = self.nodes.iter_mut();
        while let Some(node) = iter.next() {
            let other = mem::take(&mut iter).into_slice();
            for other in &mut *other {
                node.apply_forces(other, time, 30.0, true, 1.0);
            }
            iter = other.iter_mut();
        }

        for node in &mut self.nodes {
            if node.pid.is_none() {
                continue;
            }
            node.update(time);
        }
    }

    fn draw(&self) {
        for edge in &self.edges {
            let start = self.nodes[edge.start];
            let end = self.nodes[edge.end];
            let protocol = &self.protocols[edge.protocol];
            start.draw_connection(&end, protocol.color, edge.protocol);
        }
        for node in &self.nodes {
            node.draw();
        }
    }
}

#[derive(Clone, Copy)]
struct WorldRc(&'static RefCell<World>);

impl Default for WorldRc {
    fn default() -> Self {
        Self(Box::leak(Box::new(RefCell::new(World::default()))))
    }
}

impl topology_wrapper::collector::World for WorldRc {
    fn handle_update(&mut self, peer: PeerId, update: topology_wrapper::report::Update) {
        fn by_peer_id(nodes: &[Node], peer: PeerId) -> Option<usize> {
            nodes.iter().position(|node| node.pid == Some(peer))
        }

        let mut s = self.0.borrow_mut();

        let (width, height) = (screen_width(), screen_height());

        let index = by_peer_id(&s.nodes, peer)
            .unwrap_or_else(|| s.add_node(Node::new(width / 2.0, height / 2.0, peer)));
        let Some(other) = by_peer_id(&s.nodes, update.peer) else {
            return;
        };

        if index == other {
            return;
        }

        use topology_wrapper::report::Event as E;
        let (start, end, protocol) = match update.event {
            E::Inbound(proto) => (index, other, proto),
            E::Outbound(proto) => (other, index, proto),
            E::Disconnected => {
                s.edges.retain(|edge| {
                    (edge.start != index || edge.end != other)
                        && (edge.start != other || edge.end != index)
                        || edge.connection != update.connection
                });
                return;
            }
        };

        let protocol = s.add_protocol(protocol);
        s.add_edge(Edge {
            start,
            end,
            protocol,
            connection: update.connection,
        });
    }

    fn disconnect(&mut self, peer: PeerId) {
        let mut s = self.0.borrow_mut();
        let Some(index) = s.nodes.iter().position(|node| node.pid == Some(peer)) else {
            return;
        };
        s.remove_node(index);
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    collector: topology_wrapper::collector::Behaviour<WorldRc>,
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
}

macro_rules! build_env {
    ($vis:vis $name:ident) => {
        #[cfg(feature = "building")]
        $vis const $name: &str = env!(stringify!($name));
        #[cfg(not(feature = "building"))]
        $vis const $name: &str = "";
    };
}

fn boot_node() -> Multiaddr {
    build_env!(pub BOOT_NODE);
    BOOT_NODE.parse().unwrap()
}

#[macroquad::main("BasicShapes")]
async fn main() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).unwrap();

    let identity = libp2p::identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(identity.public());
    let transport = websocket_websys::Transport::new(100)
        .upgrade(Version::V1)
        .authenticate(libp2p::noise::Config::new(&identity).unwrap())
        .multiplex(libp2p::yamux::Config::default())
        .boxed();
    let world = WorldRc::default();
    let behaviour = Behaviour {
        collector: topology_wrapper::collector::Behaviour::new(world),
        kad: libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id)),
        identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
            "l".into(),
            identity.public(),
        )),
    };
    let mut swarm = libp2p::Swarm::new(
        transport,
        behaviour,
        peer_id,
        libp2p::swarm::Config::with_wasm_executor(),
    );
    log::info!("Dialing {}", boot_node());
    swarm.dial(boot_node()).unwrap();

    spawn_local(async move {
        log::info!("Starting swarm");
        let mut bootstrap = None;
        let mut bootstraps_left = 3;
        loop {
            let e = swarm.select_next_some().await;
            log::debug!("{:?}", e);
            match e {
                libp2p::swarm::SwarmEvent::Behaviour(BehaviourEvent::Identify(
                    libp2p::identify::Event::Received { peer_id, info },
                )) => {
                    if let Some(addr) = info.listen_addrs.first() {
                        swarm
                            .behaviour_mut()
                            .kad
                            .add_address(&peer_id, addr.clone());
                    }
                    swarm.behaviour_mut().collector.add_peer(peer_id);
                }
                libp2p::swarm::SwarmEvent::Behaviour(BehaviourEvent::Kad(
                    libp2p::kad::Event::OutboundQueryProgressed {
                        id,
                        step: ProgressStep { last: true, .. },
                        ..
                    },
                )) if Some(id) == bootstrap && bootstraps_left != 0 => {
                    bootstraps_left -= 1;
                    #[allow(unused_assignments)]
                    {
                        bootstrap = Some(swarm.behaviour_mut().kad.bootstrap().unwrap());
                    }
                }
                libp2p::swarm::SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint: ConnectedPoint::Dialer { address, .. },
                    ..
                } => {
                    swarm.behaviour_mut().collector.add_peer(peer_id);
                    swarm.behaviour_mut().kad.add_address(&peer_id, address);
                    if bootstrap.is_none() {
                        #[allow(unused_assignments)]
                        {
                            bootstrap = Some(swarm.behaviour_mut().kad.bootstrap().unwrap());
                        }
                    }
                }
                e => log::debug!("{:?}", e),
            }
        }
    });

    loop {
        {
            let mut world = world.0.borrow_mut();
            world.update(get_frame_time());
            clear_background(BLUE);
            world.draw();
        }

        next_frame().await;
    }
}