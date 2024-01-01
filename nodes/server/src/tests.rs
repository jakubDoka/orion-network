use {
    super::*,
    libp2p::futures::{channel::mpsc, stream::FuturesUnordered, FutureExt},
    std::fmt::Debug,
};

#[tokio::test]
async fn repopulate_account() {
    let mut nodes = create_nodes(REPLICATION_FACTOR.get() + 1);
    let mut user = Account::new();
    let [mut stream, used] = Stream::new_test();
    nodes.iter_mut().next().unwrap().clients.push(used);

    stream.create_user(&mut nodes, &mut user).await;

    assert_nodes(&nodes, |node| {
        node.storage.profiles.contains_key(&user.identity())
    });

    let target = nodes.iter_mut().next().unwrap();
    target.storage.profiles.clear();
    stream
        .test_req::<chat_logic::SendMail>(&mut nodes, (user.identity(), Reminder(&[0xff])), Ok(()))
        .await;

    assert_nodes(&nodes, |node| {
        node.storage
            .profiles
            .values()
            .any(|p| unpack_messages_ref(&p.mail).next().unwrap() == [0xff])
    });

    let target = nodes.iter_mut().next().unwrap();
    target.storage.profiles.clear();
    stream
        .test_req::<chat_logic::FetchVault>(&mut nodes, user.identity(), Ok((0, 0, Reminder(&[]))))
        .await;

    assert_nodes(&nodes, |node| {
        node.storage.profiles.contains_key(&user.identity())
    });
}

#[tokio::test]
async fn direct_messaging() {
    let mut nodes = create_nodes(REPLICATION_FACTOR.get() + 1);

    let mut user = Account::new();
    let mut user2 = Account::new();
    let [mut stream1, used] = Stream::new_test();
    let [mut stream2, used2] = Stream::new_test();

    nodes.iter_mut().next().unwrap().clients.push(used);
    nodes.iter_mut().last().unwrap().clients.push(used2);
    stream1.create_user(&mut nodes, &mut user).await;
    stream2.create_user(&mut nodes, &mut user2).await;

    stream1
        .test_req::<chat_logic::SendMail>(&mut nodes, (user2.identity(), Reminder(&[1])), Ok(()))
        .await;

    stream2
        .test_req::<chat_logic::ReadMail>(&mut nodes, user2.mail_proof(), Ok(Reminder(&[0, 1, 1])))
        .await;

    stream2
        .test_req::<chat_logic::Subscribe>(&mut nodes, user2.identity().into(), Ok(()))
        .await;

    futures::future::select(
        nodes.next(),
        std::pin::pin!(tokio::time::sleep(Duration::from_millis(100))),
    )
    .await;

    stream1
        .test_req::<chat_logic::SendMail>(
            &mut nodes,
            (user2.identity(), Reminder(&[2])),
            Err(SendMailError::SentDirectly),
        )
        .await;

    stream2.expect_event(&mut nodes, Reminder(&[2])).await;

    drop(stream2);

    stream1
        .test_req::<chat_logic::SendMail>(&mut nodes, (user2.identity(), Reminder(&[3])), Ok(()))
        .await;
}

impl Stream {
    async fn test_req<P: Protocol>(
        &mut self,
        nodes: &mut FuturesUnordered<Server>,
        body: P::Request<'_>,
        expected: ProtocolResult<'_, P>,
    ) where
        for<'a> ProtocolResult<'a, P>: PartialEq + Debug,
    {
        self.inner
            .write((P::PREFIX, CallId::whatever(), body))
            .unwrap();

        response::<P>(nodes, self, 1000, expected).await;
    }

    async fn create_user(&mut self, nodes: &mut FuturesUnordered<Server>, user: &mut Account) {
        self.test_req::<CreateProfile>(
            nodes,
            (
                user.valult_proof(&[]),
                user.enc.public_key().into_bytes(),
                Reminder(&[]),
            ),
            Ok(()),
        )
        .await;
    }

    async fn expect_event<'a, T: Codec<'a> + PartialEq + Debug>(
        &mut self,
        nodes: &mut FuturesUnordered<Server>,
        expected: T,
    ) {
        futures::select! {
            _ = nodes.select_next_some() => unreachable!(),
            res = self.next().fuse() => {
                let res = res.unwrap().1.unwrap();
                {
                    let (_, resp) = <(CallId, T)>::decode(&mut unsafe { std::mem::transmute(res.as_slice()) }).unwrap();
                    assert_eq!(resp, expected);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1000)).fuse() => {
                panic!("timeout")
            }
        }
    }
}

async fn response<P: Protocol>(
    nodes: &mut FuturesUnordered<Server>,
    stream: &mut Stream,
    tiemout_milis: u64,
    expected: ProtocolResult<'_, P>,
) where
    for<'a> ProtocolResult<'a, P>: PartialEq + Debug,
{
    futures::select! {
        _ = nodes.select_next_some() => unreachable!(),
        res = stream.next().fuse() => {
            let res = res.unwrap().1.unwrap();
            {
                let (_, resp) = <(CallId, ProtocolResult<P>)>::decode(&mut unsafe { std::mem::transmute(res.as_slice()) }).unwrap();
                assert_eq!(resp, expected);
            }
        }
        _ = tokio::time::sleep(Duration::from_millis(tiemout_milis)).fuse() => {
            panic!("timeout")
        }
    }
}

#[track_caller]
fn assert_nodes(nodes: &FuturesUnordered<Server>, mut predicate: impl FnMut(&Server) -> bool) {
    assert!(nodes.iter().filter(|e| predicate(e)).count() > REPLICATION_FACTOR.get() / 2);
}

struct Account {
    sign: crypto::sign::Keypair,
    enc: crypto::enc::Keypair,
    nonce: u64,
}

impl Account {
    fn new() -> Self {
        Self {
            sign: crypto::sign::Keypair::new(),
            enc: crypto::enc::Keypair::new(),
            nonce: 0,
        }
    }

    fn valult_proof(&mut self, value: &[u8]) -> Proof {
        Proof::for_vault(&self.sign, &mut self.nonce, value)
    }

    fn mail_proof(&mut self) -> Proof {
        Proof::for_mail(&self.sign, &mut self.nonce)
    }

    fn identity(&self) -> Identity {
        crypto::hash::new(&self.sign.public_key())
    }
}

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
