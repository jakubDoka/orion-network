#[tokio::main]
async fn main() {
    config::env_config! {
        TEST_WALLETS: config::List<chain_api::AccountId>,
        BALANCE: u128,
        CHAIN_NODE: String,
    }

    let client =
        chain_api::Client::with_signer(CHAIN_NODE.as_str(), chain_api::dev_keypair("//Alice"))
            .await
            .unwrap();

    for wallet in TEST_WALLETS.0.into_iter() {
        client.transfere(wallet, BALANCE).await.unwrap();
    }
}
