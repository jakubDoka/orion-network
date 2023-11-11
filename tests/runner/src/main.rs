use std::{io, iter, process};

use clap::Parser;

#[derive(Parser)]
struct Command {
    #[clap(long, env, default_value = "10")]
    node_count: usize,
    #[clap(long, env, default_value = "8700")]
    chain_port: u16,
    #[clap(long, env, default_value = "8800")]
    first_port: u16,
    #[clap(long, env, default_value = "./target/debug/miner")]
    miner: String,
    #[clap(long, env, default_value = "./target/debug/chain-mock")]
    chain: String,
}

fn main() {
    let cmd = Command::parse();

    let chain = process::Command::new(&cmd.chain)
        .env("PORT", cmd.chain_port.to_string())
        .stdout(process::Stdio::inherit())
        .stderr(process::Stdio::inherit())
        .spawn()
        .expect("failed to spawn child");

    let children = (0..cmd.node_count)
        .map(|i| {
            println!("Starting node {i} ({})", cmd.miner);
            process::Command::new(&cmd.miner)
                .env("PORT", (cmd.first_port + i as u16).to_string())
                .env("CHAIN_PORT", cmd.chain_port.to_string())
                .stdout(process::Stdio::inherit())
                .stderr(process::Stdio::inherit())
                .spawn()
                .expect("failed to spawn child")
        })
        .chain(iter::once(chain))
        .collect::<Vec<_>>();

    io::stdin().read_line(&mut String::new()).unwrap();

    for mut child in children {
        child.kill().unwrap();
    }
}
