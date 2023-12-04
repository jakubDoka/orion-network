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

#[derive(Default)]
struct Nodes {
    inner: Vec<Option<Node>>,
    free: Vec<usize>,
}

impl Nodes {
    fn push(&mut self, node: Node) -> usize {
        if let Some(index) = self.free.pop() {
            self.inner[index] = Some(node);
            return index;
        }

        let index = self.inner.len();
        self.inner.push(Some(node));
        index
    }

    fn remove(&mut self, index: usize) {
        self.inner[index] = None;
        self.free.push(index);
    }

    fn get(&self, index: usize) -> Option<&Node> {
        self.inner[index].as_ref()
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Node> {
        self.inner[index].as_mut()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = (usize, &mut Node)> + '_ {
        self.inner
            .iter_mut()
            .enumerate()
            .filter_map(|(i, node)| Some((i, node.as_mut()?)))
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn retain(&mut self, mut keep: impl FnMut(&mut Node) -> bool) {
        for (i, node) in self.inner.iter_mut().enumerate() {
            let Some(nd) = node else {
                continue;
            };
            if !keep(nd) {
                *node = None;
                self.free.push(i);
            }
        }
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &Node)> + '_ {
        self.inner
            .iter()
            .enumerate()
            .filter_map(|(i, node)| Some((i, node.as_ref()?)))
    }
}

#[derive(Debug, Clone, Copy)]
struct Node {
    pid: PeerId,
    position: Vec2,
    velocity: Vec2,
    seen: bool,
    client: bool,
}

impl Node {
    const FRICION: f32 = 0.1;
    const LINE_THICKNESS: f32 = 3.0;
    const NODE_SIZE: f32 = 20.0;

    fn new(x: f32, y: f32, client: bool, pid: PeerId) -> Self {
        Self {
            pid,
            position: Vec2::new(x, y),
            velocity: Vec2::from_angle(rand::gen_range(0.0, 2.0 * std::f32::consts::PI)) * 300.0,
            seen: false,
            client,
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
        let color = if self.client { GREEN } else { RED };
        draw_circle(self.position.x, self.position.y, Self::NODE_SIZE, color);
    }

    fn draw_connection(&self, other: &Self, color: Color, index: usize, total_protocols: usize) {
        let dir = other.position - self.position;
        let offset = (total_protocols - index) as f32 - total_protocols as f32 / 2.0;
        let dir = Vec2::new(dir.y, -dir.x);
        let offset = dir.normalize() * offset * Self::LINE_THICKNESS;
        draw_line(
            self.position.x + offset.x,
            self.position.y + offset.y,
            other.position.x + offset.x,
            other.position.y + offset.y,
            Self::LINE_THICKNESS,
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
    nodes: Nodes,
    edges: BTreeSet<Edge>,
    protocols: Vec<Protocol>,
}

#[allow(dead_code)]
impl World {
    fn add_node(&mut self, node: Node) -> usize {
        self.nodes.push(node)
    }

    fn add_protocol(&mut self, protocol: &str) -> usize {
        if let Some(index) = self.protocols.iter().position(|p| p.name == protocol) {
            return index;
        }

        let index = self.protocols.len();
        let rand_config = rand::gen_range(u32::MIN, u32::MAX);
        self.protocols.push(Protocol {
            name: protocol.to_owned(),
            color: Color::new(
                (rand_config >> 24) as u8 as f32 / 255.0,
                (rand_config >> 16) as u8 as f32 / 255.0,
                (rand_config >> 8) as u8 as f32 / 255.0,
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
        self.nodes.remove(index);
        self.edges
            .retain(|edge| edge.start != index && edge.end != index);
    }

    fn update(&mut self, time: f32) {
        self.nodes
            .iter_mut()
            .for_each(|(_, node)| node.seen = false);
        let (mut prev_start, mut prev_end) = (self.nodes.len(), self.edges.len());
        self.edges.retain(|edge| {
            if edge.start == prev_start && edge.end == prev_end {
                return true;
            }
            prev_start = edge.start;
            prev_end = edge.end;

            let (Some(mut start), Some(mut end)) = (
                self.nodes.get(edge.start).cloned(),
                self.nodes.get(edge.end).cloned(),
            ) else {
                return false;
            };
            start.seen = true;
            end.seen = true;
            start.apply_forces(&mut end, time, 120.0, false, 0.05);
            *self.nodes.get_mut(edge.start).unwrap() = start;
            *self.nodes.get_mut(edge.end).unwrap() = end;
            true
        });
        self.nodes.retain(|node| node.seen);

        let mut iter = self.nodes.inner.iter_mut();
        while let Some(node) = iter.by_ref().find_map(Option::as_mut) {
            let other = mem::take(&mut iter).into_slice();
            for other in other.iter_mut().filter_map(Option::as_mut) {
                node.apply_forces(other, time, 60.0, true, 1.0);
            }
            iter = other.iter_mut();
        }

        for (_, node) in self.nodes.iter_mut() {
            node.update(time);
        }
    }

    fn draw(&self) {
        for edge in &self.edges {
            let (Some(start), Some(end)) = (self.nodes.get(edge.start), self.nodes.get(edge.end))
            else {
                continue;
            };
            let protocol = &self.protocols[edge.protocol];
            start.draw_connection(end, protocol.color, edge.protocol, self.protocols.len());
        }

        for (_, node) in self.nodes.iter() {
            node.draw();
        }

        let mut cursor = 30.0;

        for protocol in &self.protocols {
            draw_text(&protocol.name, 10.0, cursor, 30.0, protocol.color);
            cursor += 30.0;
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

fn by_peer_id(nodes: &Nodes, peer: PeerId) -> Option<usize> {
    nodes
        .iter()
        .find_map(|(id, node)| (node.pid == peer).then_some(id))
}

impl topology_wrapper::collector::World for WorldRc {
    fn handle_update(
        &mut self,
        peer: PeerId,
        update: topology_wrapper::report::Update,
        client: bool,
    ) {
        let mut s = self.0.borrow_mut();

        let (width, height) = (screen_width(), screen_height());

        let index = by_peer_id(&s.nodes, peer)
            .unwrap_or_else(|| s.add_node(Node::new(width / 2.0, height / 2.0, client, peer)));
        let other = by_peer_id(&s.nodes, update.peer).unwrap_or_else(|| {
            s.add_node(Node::new(width / 2.0, height / 2.0, client, update.peer))
        });

        if index == other {
            return;
        }

        s.nodes.get_mut(index).unwrap().client &= client;
        s.nodes.get_mut(other).unwrap().client &= client;

        use topology_wrapper::report::Event as E;
        let protocol = match update.event {
            E::Stream(p) => p,
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
            start: index.max(other),
            end: index.min(other),
            protocol,
            connection: update.connection,
        });
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
        collector: topology_wrapper::collector::Behaviour::new(peer_id, world),
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
        let mut bootstraps_left = 5;
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

    let mut dragged_node = None;
    loop {
        {
            let mut world = world.0.borrow_mut();

            if is_mouse_button_pressed(MouseButton::Left) {
                let (x, y) = mouse_position();

                dragged_node = world.nodes.iter_mut().find_map(|(id, node)| {
                    let diff = Vec2::new(x, y) - node.position;
                    (diff.length_squared() < Node::NODE_SIZE * Node::NODE_SIZE).then_some(id)
                });
            }

            if is_mouse_button_released(MouseButton::Left) {
                dragged_node = None;
            }

            if let Some(dragged_node) = dragged_node {
                let (x, y) = mouse_position();
                if let Some(node) = world.nodes.get_mut(dragged_node) {
                    node.position = Vec2::new(x, y);
                }
            }

            world.update(get_frame_time());

            if let Some(dragged_node) = dragged_node {
                let (x, y) = mouse_position();
                if let Some(node) = world.nodes.get_mut(dragged_node) {
                    node.position = Vec2::new(x, y);
                }
            }

            clear_background(BLACK);
            world.draw();
        }

        next_frame().await;
    }
}
