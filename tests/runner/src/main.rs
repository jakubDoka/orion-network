use clap::Parser;
use libp2p::identity::ed25519::Keypair;
use x25519_dalek::StaticSecret;

#[derive(Parser)]
struct Command {
    #[clap(long, env, default_value = "10")]
    node_count: usize,
    #[clap(long, env, default_value = "8800")]
    first_port: u16,
    #[clap(long, env, default_value = "target/release/miner")]
    miner: String,
}

fn main() {
    let cmd = Command::parse();

    let onion_secrets = (0..cmd.node_count)
        .map(|_| StaticSecret::random())
        .collect::<Vec<_>>();
    let secrets = (0..cmd.node_count)
        .map(|_| Keypair::generate())
        .collect::<Vec<_>>();

    for (i, (onion_secret, secret)) in onion_secrets.into_iter().zip(secrets).enumerate() {
        let onion_secret = onion_secret.to_bytes();
        let secret: [u8; 32] = unsafe { std::mem::transmute(secret.secret()) };

        let child = std::process::Command::new(&cmd.miner)
            .env("PORT", (cmd.first_port + i as u16).to_string())
            .env("SECRET", hex::encode(secret))
            .env("ONION_SECRET", hex::encode(onion_secret))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("failed to spawn child");

        std::thread::spawn(move || {
            child.wait_with_output().unwrap();
        });
    }

    std::io::stdin().read_line(&mut String::new()).unwrap();
}
