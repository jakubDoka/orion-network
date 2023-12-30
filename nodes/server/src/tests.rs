use {
    super::*,
    crate::handlers::Request,
    libp2p::futures::{channel::mpsc, stream::FuturesUnordered},
};

fn next_node_config() -> NodeConfig {
    static PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let port = PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    NodeConfig {
        port: port * 2 + 5000,
        ws_port: port * 2 + 1 + 5000,
        key_path: Default::default(),
        boot_nodes: config::List::default(),
        idle_timeout: 1000,
    }
}

fn create_nodes(count: usize) -> FuturesUnordered<Server> {
    let node_data = (0..count)
        .map(|_| (next_node_config(), NodeKeys::default()))
        .collect::<Vec<_>>();

    let nodes = node_data
        .iter()
        .map(|(config, keys)| {
            (
                keys.to_stored(),
                (IpAddr::from(Ipv4Addr::LOCALHOST), config.port).into(),
            )
        })
        .collect::<Vec<_>>();

    node_data
        .into_iter()
        .map(|(config, keys)| {
            let (_, rx) = mpsc::channel(1);
            Server::new(config, keys, nodes.clone(), rx).unwrap()
        })
        .collect()
}

fn new_request<'a, P: Protocol>(body: P::Request<'_>, owner: &'a mut Vec<u8>) -> Request<'a> {
    owner.clear();
    body.encode(owner).unwrap();
    Request {
        prefix: <P as Protocol>::PREFIX,
        id: CallId::new(),
        origin: RequestOrigin::Client(PathId::new()),
        body: owner.as_slice(),
    }
}

#[tokio::test]
async fn repopulate_account() {
    env_logger::init();

    let mut nodes = create_nodes(REPLICATION_FACTOR.get() + 1);

    let sign = crypto::sign::Keypair::new();
    let enc = crypto::enc::Keypair::new();
    let mut nonce = 0;
    let account_proof = Proof::for_vault(&sign, &mut nonce, &[]);
    let entry = nodes.iter_mut().next().unwrap();
    let mut buffer = vec![];
    let mut request_buffer = vec![];
    entry
        .external
        .execute(
            crate::extract_ctx!(entry),
            new_request::<CreateProfile>(
                (account_proof, enc.public_key().into_bytes(), Reminder(&[])),
                &mut request_buffer,
            ),
            &mut buffer,
        )
        .unwrap();

    futures::future::select(
        nodes.next(),
        std::pin::pin!(tokio::time::sleep(std::time::Duration::from_secs(1))),
    )
    .await;

    let mut missing = vec![];
    for (i, node) in nodes.iter_mut().enumerate() {
        if node.storage.profiles.is_empty() {
            missing.push(i);
        }
    }
    assert_eq!(missing.as_slice(), &[0usize][..0]);

    let target = nodes.iter_mut().next().unwrap();
    target.storage.profiles.clear();
    let mut buffer = vec![];
    let mut request_buffer = vec![];
    target
        .external
        .execute(
            crate::extract_ctx!(target),
            new_request::<chat_logic::SendMail>(
                (crypto::hash::new(&sign.public_key()), Reminder(&[0xff])),
                &mut request_buffer,
            ),
            &mut buffer,
        )
        .unwrap();

    futures::future::select(
        nodes.next(),
        std::pin::pin!(tokio::time::sleep(std::time::Duration::from_secs(1))),
    )
    .await;

    let mut missing = vec![];
    for (i, node) in nodes.iter_mut().enumerate() {
        if node
            .storage
            .profiles
            .values()
            .all(|p| unpack_messages_ref(&p.mail).next().unwrap() != [0xff])
        {
            missing.push(i);
        }
    }
    assert_eq!(missing.as_slice(), &[0usize][..0]);
}
