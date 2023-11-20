#![feature(lazy_cell)]
use std::str::FromStr;
use std::u64;

use parity_scale_codec::{Decode, Encode as _};
use polkadot::contracts::calls::types::Call;
use polkadot::runtime_types::pallet_contracts_primitives::{ContractResult, ExecReturnValue};
use polkadot::runtime_types::sp_runtime::DispatchError;
use polkadot::runtime_types::sp_weights::weight_v2::Weight;
use protocols::contracts::{Identity, NodeData, SerializedNodeData};
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::RpcClient;
use subxt::tx::{Payload, Signer};
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::bip39::Mnemonic;
use subxt_signer::sr25519::dev;

pub type Config = PolkadotConfig;
pub type Balance = u128;
pub type ContractId = <Config as subxt::Config>::AccountId;
pub type AccountId = <Config as subxt::Config>::AccountId;
pub type Error = subxt::Error;
pub type Keypair = subxt_signer::sr25519::Keypair;

#[track_caller]
pub fn dev_keypair(name: &str) -> Keypair {
    subxt_signer::sr25519::Keypair::from_uri(&subxt_signer::SecretUri::from_str(name).unwrap())
        .unwrap()
}

#[track_caller]
pub fn mnemonic_keypair(mnemonic: &str) -> Keypair {
    subxt_signer::sr25519::Keypair::from_phrase(&Mnemonic::from_str(mnemonic).unwrap(), None)
        .unwrap()
}

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
//impl Signer<Config> for MySigner {
//    fn account_id(&self) -> AccountId {
//        self.0.public_key().into()
//    }
//
//    fn address(&self) -> MultiAddress<AccountId, ()> {
//        self.0.public_key().into()
//    }
//
//    fn sign(&self, signer_payload: &[u8]) -> subxt::utils::MultiSignature {
//        self.0.sign(signer_payload).into()
//    }
//}

pub struct Client {
    account: Keypair,
    client: OnlineClient<Config>,
    legacy: LegacyRpcMethods<Config>,
}

impl Client {
    pub async fn new(url: &str, account: Keypair) -> Result<Self, subxt::Error> {
        let rpc = RpcClient::from_url(url).await?;
        let client = OnlineClient::<Config>::from_rpc_client(rpc.clone()).await?;
        let legacy = LegacyRpcMethods::new(rpc);

        Ok(Self {
            account,
            client,
            legacy,
        })
    }

    pub async fn default() -> Result<Self, subxt::Error> {
        let url = "ws://localhost:9944";
        let account = dev::alice();
        Self::new(url, account).await
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
            .account_nonce(&Signer::<Config>::account_id(&self.account))
            .await?;
        Ok(account_nonce)
    }

    pub async fn join(&self, dest: ContractId, data: NodeData) -> Result<(), subxt::Error> {
        self.call_auto_witght(
            1000000,
            dest,
            contracts::node_staker::messages::join(data.into()),
        )
        .await
    }

    pub async fn list(&self, addr: ContractId) -> Result<Vec<SerializedNodeData>, subxt::Error> {
        self.make_dry_call::<Result<_, _>>(CallRequest {
            origin: Signer::<Config>::account_id(&self.account),
            dest: addr,
            value: 0,
            gas_limit: None,
            storage_deposit_limit: None,
            input_data: contracts::node_staker::messages::list().to_bytes(),
        })
        .await
        .and_then(|(res, _)| {
            res.map_err(|()| subxt::Error::Other(format!("Contract call returned error")))
        })
    }

    pub async fn vote(
        &self,
        dest: ContractId,
        me: protocols::contracts::Identity,
        target: protocols::contracts::Identity,
        weight: i32,
    ) -> Result<(), subxt::Error> {
        let call = contracts::node_staker::messages::vote(me, target, weight);
        self.call_auto_witght(0, dest, call).await
    }

    pub async fn reclaim(&self, dest: ContractId, me: Identity) -> Result<(), subxt::Error> {
        self.call_auto_witght(0, dest, contracts::node_staker::messages::reclaim(me))
            .await
    }

    async fn call_auto_witght<T: parity_scale_codec::Decode>(
        &self,
        value: Balance,
        dest: ContractId,
        call_data: impl InkMessage,
    ) -> Result<T, subxt::Error> {
        let (res, weight) = self
            .make_dry_call(CallRequest {
                origin: Signer::<Config>::account_id(&self.account),
                dest: dest.clone(),
                value,
                gas_limit: None,
                storage_deposit_limit: None,
                input_data: call_data.to_bytes(),
            })
            .await?;

        self.make_call(polkadot::tx().contracts().call(
            dest.into(),
            value,
            weight,
            None,
            call_data.to_bytes(),
        ))
        .await?;

        Ok(res)
    }

    async fn make_dry_call<T: parity_scale_codec::Decode>(
        &self,
        call: CallRequest,
    ) -> Result<(T, Weight), subxt::Error> {
        let bytes = call.encode();
        self.legacy
            .state_call("ContractsApi_call", Some(&bytes), None)
            .await
            .and_then(|r| {
                <ContractResult<Result<ExecReturnValue, DispatchError>, Balance, ()>>::decode(
                    &mut r.as_slice(),
                )
                .map_err(|e| subxt::Error::Decode(subxt::error::DecodeError::custom(e)))
            })
            .and_then(|r| {
                let res = r
                    .result
                    .map_err(|e| subxt::Error::Other(format!("Contract error: {:?}", e)))?;
                let res = T::decode(&mut res.data.as_slice())
                    .map_err(|e| subxt::Error::Decode(subxt::error::DecodeError::custom(e)))?;
                Ok((res, r.gas_consumed))
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
            .map(drop)
    }
}

/// A struct that encodes RPC parameters required for a call to a smart contract.
///
/// Copied from `pallet-contracts-rpc-runtime-api`.
#[derive(parity_scale_codec::Encode)]
struct CallRequest {
    origin: AccountId,
    dest: AccountId,
    value: Balance,
    gas_limit: Option<Weight>,
    storage_deposit_limit: Option<Balance>,
    input_data: Vec<u8>,
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
