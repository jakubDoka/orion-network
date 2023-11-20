#![feature(lazy_cell)]
use std::net::Ipv4Addr;
use std::ops::Deref;
use std::path::Path;
use std::str::FromStr;
use std::sync::LazyLock;
use std::u64;

use blake2::{Blake2s256, Digest};
use contract_transcode::ContractMessageTranscoder;
use parity_scale_codec::{Decode, Encode as _};
use polkadot::contracts::calls::types::Call;
use polkadot::runtime_types::pallet_contracts_primitives::{ContractResult, ExecReturnValue};
use polkadot::runtime_types::sp_runtime::DispatchError;
use polkadot::runtime_types::sp_weights::weight_v2::Weight;
use polkadot::system::events::ExtrinsicFailed;
use protocols::contracts::NodeData;
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::RpcClient;
use subxt::tx::{Payload, Signer};
use subxt::utils::AccountId32;
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::sr25519::{dev, Keypair};

pub const NODES: &str = "/nodes";

pub const USER_BY_NAME: &str = "/user/name/:id";
pub const USER_BY_SIGN: &str = "/user/sign/:id";
pub const CREATE_USER: &str = "/user";

pub type ContractId = AccountId32;

/// Trait implemented by [`smart_bench_macro::contract`] for all contract constructors.
pub trait InkConstructor: parity_scale_codec::Encode {
    const SELECTOR: [u8; 4];

    fn to_bytes(&self) -> Vec<u8> {
        let mut call_data = Self::SELECTOR.to_vec();
        <Self as parity_scale_codec::Encode>::encode_to(self, &mut call_data);
        call_data
    }
}

/// Trait implemented by [`smart_bench_macro::contract`] for all contract messages.
pub trait InkMessage: parity_scale_codec::Encode {
    const SELECTOR: [u8; 4];

    fn to_bytes(&self) -> Vec<u8> {
        let mut call_data = Self::SELECTOR.to_vec();
        <Self as parity_scale_codec::Encode>::encode_to(self, &mut call_data);
        call_data
    }
}

#[subxt::subxt(runtime_metadata_path = "metadata.scale")]
mod polkadot {}
mod contracts {
    contract_macro::contract!("../../target/ink/node_staker/node_staker.contract");
}

//struct MySigner(pub subxt_signer::sr25519::Keypair);
//
//impl Signer<PolkadotConfig> for MySigner {
//    fn account_id(&self) -> AccountId32 {
//        self.0.public_key().into()
//    }
//
//    fn address(&self) -> MultiAddress<AccountId32, ()> {
//        self.0.public_key().into()
//    }
//
//    fn sign(&self, signer_payload: &[u8]) -> subxt::utils::MultiSignature {
//        self.0.sign(signer_payload).into()
//    }
//}

async fn get_account_nonce(
    client: &OnlineClient<PolkadotConfig>,
    rpc: &LegacyRpcMethods<PolkadotConfig>,
    account_id: &AccountId32,
) -> core::result::Result<u64, subxt::Error> {
    let best_block = rpc
        .chain_get_block_hash(None)
        .await?
        .ok_or(subxt::Error::Other("Best block not found".into()))?;
    let account_nonce = client
        .blocks()
        .at(best_block)
        .await?
        .account_nonce(account_id)
        .await?;
    Ok(account_nonce)
}

pub struct Client {
    account: Keypair,
    client: OnlineClient<PolkadotConfig>,
    legacy: LegacyRpcMethods<PolkadotConfig>,
}

impl Client {
    pub async fn new(url: &str, account: Keypair) -> Result<Self, subxt::Error> {
        let rpc = RpcClient::from_url(url).await?;
        let client = OnlineClient::<PolkadotConfig>::from_rpc_client(rpc.clone()).await?;
        let legacy = LegacyRpcMethods::new(rpc);

        Ok(Self {
            account,
            client,
            legacy,
        })
    }

    async fn get_nonce(&self) -> Result<u64, subxt::Error> {
        let best_block = self
            .legacy
            .chain_get_block_hash(None)
            .await?
            .ok_or(subxt::Error::Other("Best block not found".into()))?;
        let account_nonce = self
            .client
            .blocks()
            .at(best_block)
            .await?
            .account_nonce(&Signer::<PolkadotConfig>::account_id(&self.account))
            .await?;
        Ok(account_nonce)
    }

    pub async fn join(&self, addr: ContractId, data: NodeData) -> Result<(), subxt::Error> {
        let res = polkadot::tx().contracts().call(
            addr.into(),
            1000000,
            Weight {
                ref_time: 1000000000,
                proof_size: 1000000,
            },
            None,
            contracts::node_staker::messages::join(crypto::sign::PublicKey::from(data.sign).ed)
                .to_bytes(),
        );

        self.make_call(res).await
    }

    pub async fn list(&self, addr: ContractId) -> Result<Result<Vec<[u8; 32]>, ()>, subxt::Error> {
        self.make_dry_call(CallRequest {
            origin: Signer::<PolkadotConfig>::account_id(&self.account),
            dest: addr,
            value: 0,
            gas_limit: None,
            storage_deposit_limit: None,
            input_data: contracts::node_staker::messages::list().to_bytes(),
        })
        .await
    }

    async fn make_dry_call<T: parity_scale_codec::Decode>(
        &self,
        call: CallRequest,
    ) -> Result<T, subxt::Error> {
        let bytes = call.encode();
        self.legacy
            .state_call("ContractsApi_call", Some(&bytes), None)
            .await
            .and_then(|r| {
                <ContractResult<Result<ExecReturnValue, DispatchError>, u128, ()>>::decode(
                    &mut r.as_slice(),
                )
                .map_err(|e| subxt::Error::Decode(subxt::error::DecodeError::custom(e)))
            })
            .and_then(|r| {
                r.result
                    .map_err(|e| subxt::Error::Other(format!("Contract error: {:?}", e)))
            })
            .and_then(|r| {
                T::decode(&mut r.data.as_slice())
                    .map_err(|e| subxt::Error::Decode(subxt::error::DecodeError::custom(e)))
            })
    }

    async fn make_call(&self, call: Payload<Call>) -> Result<(), subxt::Error> {
        let nonce = self.get_nonce().await?;

        self.client
            .tx()
            .create_signed_with_nonce(&call, &self.account, nonce, Default::default())?
            .submit_and_watch()
            .await?
            .wait_for_in_block()
            .await?
            .wait_for_success()
            .await
            .map(|e| {
                for e in e.iter() {
                    let e = e.unwrap();
                    // dbg!(e.event_metadata().pallet);

                    if let Ok(Some(e)) =
                        e.as_event::<polkadot::contracts::events::ContractEmitted>()
                    {
                        dbg!(e.contract);
                        dbg!(e.data);
                    } else if let Ok(Some(e)) = e.as_event::<polkadot::contracts::events::Called>()
                    {
                        dbg!(e.contract);
                        dbg!(e.caller);
                    } else if let Ok(Some(e)) =
                        e.as_event::<polkadot::system::events::ExtrinsicSuccess>()
                    {
                        dbg!(e.dispatch_info);
                    } else {
                        dbg!(e.event_metadata().variant);
                    }
                }
            })
    }
}

/// A struct that encodes RPC parameters required for a call to a smart contract.
///
/// Copied from `pallet-contracts-rpc-runtime-api`.
#[derive(parity_scale_codec::Encode)]
struct CallRequest {
    origin: AccountId32,
    dest: AccountId32,
    value: u128,
    gas_limit: Option<Weight>,
    storage_deposit_limit: Option<u128>,
    input_data: Vec<u8>,
}

#[tokio::test]
async fn test() {
    let cid = AccountId32::from_str("5HkkPTgt6JYUfVKuoBmamXbctXQTpwCH4WqX8FtEdNapWrXK").unwrap();

    let data = NodeData {
        sign: [6; 1984],
        enc: [0; 1600],
        ip: Ipv4Addr::LOCALHOST,
        port: Default::default(),
    };

    let client = Client::new("ws://localhost:9944", dev::alice())
        .await
        .unwrap();

    //client.join(cid, data).await.unwrap();
    dbg!(client.list(cid).await.unwrap());
}

//
//#[derive(Debug, thiserror::Error)]
//pub enum RegisterNodeError {
//    #[error(transparent)]
//    Reqwest(#[from] reqwest::Error),
//    #[error("node already exists")]
//    Conflict,
//}
//
//pub async fn nodes(addr: impl fmt::Display) -> Result<Vec<NodeData>, NodesError> {
//    let url = format!("{addr}{NODES}");
//    let res = get_client().get(&url).send().await?;
//    match res.status() {
//        reqwest::StatusCode::OK => {}
//        _ => return Err(res.error_for_status().unwrap_err().into()),
//    }
//    let data = res.bytes().await?;
//    <Vec<NodeData>>::decode(&mut data.as_ref()).ok_or(NodesError::Codec)
//}
//
//#[derive(Debug, thiserror::Error)]
//pub enum NodesError {
//    #[error(transparent)]
//    Reqwest(#[from] reqwest::Error),
//    #[error("failed to decode nodes data")]
//    Codec,
//}
//
//pub async fn create_user(addr: impl fmt::Display, data: UserData) -> Result<(), CreateUserError> {
//    let url = format!("{addr}{CREATE_USER}");
//    let res = get_client().post(&url).body(data.to_bytes()).send().await?;
//    match res.status() {
//        reqwest::StatusCode::OK => Ok(()),
//        reqwest::StatusCode::CONFLICT => Err(CreateUserError::Conflict),
//        _ => Err(res.error_for_status().unwrap_err().into()),
//    }
//}
//
//#[derive(Debug, thiserror::Error)]
//pub enum CreateUserError {
//    #[error(transparent)]
//    Reqwest(#[from] reqwest::Error),
//    #[error("user already exists")]
//    Conflict,
//}
//
//pub async fn user_by_name(
//    addr: impl fmt::Display,
//    name: UserName,
//) -> Result<UserData, GetUserError> {
//    let url = format!("{addr}{}", USER_BY_NAME.replace(":id", &name));
//    get_user(url).await
//}
//
//pub async fn user_by_sign(
//    addr: impl fmt::Display,
//    sign: crypto::sign::PublicKey,
//) -> Result<UserData, GetUserError> {
//    let hex_sign = hex::encode(sign.ed);
//    let url = format!("{addr}{}", USER_BY_SIGN.replace(":id", &hex_sign));
//    get_user(url).await
//}
//
//async fn get_user(path: String) -> Result<UserData, GetUserError> {
//    let res = get_client().get(&path).send().await?;
//    match res.status() {
//        reqwest::StatusCode::OK => {}
//        reqwest::StatusCode::NOT_FOUND => return Err(GetUserError::NotFound),
//        _ => return Err(GetUserError::Reqwest(res.error_for_status().unwrap_err())),
//    }
//    let data = res.bytes().await?;
//    UserData::decode(&mut data.as_ref()).ok_or(GetUserError::Codec)
//}
//
//#[derive(Debug, thiserror::Error)]
//pub enum GetUserError {
//    #[error(transparent)]
//    Reqwest(#[from] reqwest::Error),
//    #[error("failed to decode user data")]
//    Codec,
//    #[error("user not found")]
//    NotFound,
//}
