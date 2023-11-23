use std::{io, iter, process, thread::sleep};

use clap::Parser;

#[derive(Parser)]
struct Command {
    #[clap(long, env, default_value = "10")]
    node_count: usize,
    #[clap(long, env, default_value = "8800")]
    first_port: u16,
    #[clap(long, env, default_value = "./target/debug/miner")]
    miner: String,
}

fn main() {
    let cmd = Command::parse();

    let accounts = ["Alice", "Bob", "Charlie", "Dave", "Eve", "Ferdie"];
    let children = (0..cmd.node_count)
        .map(|i| {
            println!("Starting node {i} ({})", cmd.miner);
            if i % accounts.len() == 0 && i != 0 {
                sleep(std::time::Duration::from_secs(7));
            }
            process::Command::new(&cmd.miner)
                .env("PORT", (cmd.first_port + i as u16).to_string())
                .env("NODE_ACCOUNT", format!("//{}", accounts[i % accounts.len()]))
                .stdout(process::Stdio::inherit())
                .stderr(process::Stdio::inherit())
                .spawn()
                .expect("failed to spawn child")
        })
        .collect::<Vec<_>>();

    sleep(std::time::Duration::MAX);
}
