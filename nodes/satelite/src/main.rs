use {
    anyhow::Context,
    component_utils::futures::StreamExt,
    libp2p::swarm::{NetworkBehaviour, SwarmEvent},
    std::net::Ipv4Addr,
};

type Db = storage_spec::db::MemoryDatabase;
fn connect_db(_: &Config) -> anyhow::Result<Db> {
    Ok(storage_spec::db::MemoryDatabase::default())
}

config::env_config! {
    struct Config {
        port: u16 = "8080",
        external_ip: Ipv4Addr = "127.0.0.1",
        identity_ed: config::Hex = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env();
    let db = connect_db(&config)?;
    let mut satelite = Satelite::new(config, db)?;
    satelite.run().await
}

pub struct Satelite {
    swarm: libp2p::Swarm<Behaviour>,
    db: Db,
}

impl Satelite {
    fn new(config: Config, db: Db) -> anyhow::Result<Self> {
        let identity: libp2p::identity::Keypair =
            libp2p::identity::ed25519::Keypair::try_from_bytes(&mut config.identity_ed.to_bytes())
                .context("invalid identity")?
                .into();

        let swarm = libp2p::SwarmBuilder::with_existing_identity(identity)
            .with_tokio()
            .with_quic()
            .with_behaviour(|_| Behaviour::default())?
            .with_swarm_config(|c| {
                c.with_idle_connection_timeout(std::time::Duration::from_secs(60))
            })
            .build();

        Ok(Self { swarm, db })
    }

    async fn run(&mut self) -> ! {
        loop {
            tokio::select! {
                e = self.swarm.select_next_some() => self.swarm_event(e),
            }
        }
    }

    fn swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) {}
}

#[derive(NetworkBehaviour, Default)]
pub struct Behaviour {
    dht: dht::Behaviour,
    rpc: rpc::Behaviour,
    streaming: streaming::Behaviour,
}
