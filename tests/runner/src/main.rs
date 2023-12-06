use {
    clap::Parser,
    std::{fmt::Write, process, thread::sleep},
};

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
    let _children = (0..cmd.node_count)
        .map(|i| {
            println!("Starting node {i} ({})", cmd.miner);
            if i % accounts.len() == 0 && i != 0 {
                sleep(std::time::Duration::from_secs(7));
            }
            let mut command = process::Command::new(&cmd.miner);
            let mut boot_nodes = String::new();
            for _ in 1..cmd.node_count {
                if let Some(i) = i.checked_sub(1) {
                    write!(
                        boot_nodes,
                        "/ip4/127.0.0.1/tcp/{},",
                        cmd.first_port + i as u16
                    )
                    .unwrap();
                }
            }
            let child = command
                .env("PORT", (cmd.first_port + i as u16).to_string())
                .env("WS_PORT", (cmd.first_port + i as u16 + 100).to_string())
                .env("BOOT_NODES", boot_nodes)
                .env(
                    "NODE_ACCOUNT",
                    format!("//{}", accounts[i % accounts.len()]),
                )
                .env("KEY_PATH", format!("node_keys/node{i}.keys"))
                .stdout(process::Stdio::piped())
                .stderr(process::Stdio::piped())
                .spawn()
                .expect("failed to spawn child");

            let log_file = format!("node_logs/node{}.log", i);
            let mut log_file = std::fs::File::create(log_file).unwrap();
            let stderr = child.stderr.unwrap();
            let mut stderr = std::io::BufReader::new(stderr);
            std::thread::spawn(move || std::io::copy(&mut stderr, &mut log_file).unwrap());
        })
        .collect::<Vec<_>>();

    sleep(std::time::Duration::MAX);
}
