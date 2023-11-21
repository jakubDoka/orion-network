#![feature(lazy_cell)]
use std::str::FromStr;
use std::u64;

use parity_scale_codec::{Decode, Encode as _};
use polkadot::contracts::calls::types::Call;
use polkadot::runtime_types::pallet_contracts_primitives::{ContractResult, ExecReturnValue};
use polkadot::runtime_types::sp_runtime::DispatchError;
use polkadot::runtime_types::sp_weights::weight_v2::Weight;
use protocols::contracts::SerializedUserIdentity;
use protocols::contracts::{Identity, NodeData, SerializedNodeData};
use protocols::UserName;
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::RpcClient;
use subxt::tx::Payload;
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::bip39::Mnemonic;

pub type Config = PolkadotConfig;
pub type Balance = u128;
pub type ContractId = <Config as subxt::Config>::AccountId;
pub type AccountId = <Config as subxt::Config>::AccountId;
pub type Error = subxt::Error;
pub type Keypair = subxt_signer::sr25519::Keypair;
pub type Signature = subxt_signer::sr25519::Signature;

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

pub fn new_signature(sig: [u8; 64]) -> Signature {
    subxt_signer::sr25519::Signature(sig)
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
    contract_macro::contract!("../../target/ink/user_manager/user_manager.contract");
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

#[allow(async_fn_in_trait)]
pub trait AsyncSigner {
    async fn sign_async(&self, signer_payload: &[u8]) -> Result<Signature, Error>;
    async fn account_id_async(&self) -> Result<AccountId, Error>;
}

impl AsyncSigner for Keypair {
    async fn sign_async(&self, signer_payload: &[u8]) -> Result<Signature, Error> {
        Ok(self.sign(signer_payload))
    }

    async fn account_id_async(&self) -> Result<AccountId, Error> {
        Ok(self.public_key().into())
    }
}

pub struct UnreachableSigner;

impl AsyncSigner for UnreachableSigner {
    async fn sign_async(&self, _: &[u8]) -> Result<Signature, Error> {
        unreachable!()
    }

    async fn account_id_async(&self) -> Result<AccountId, Error> {
        unreachable!()
    }
}

pub struct Client<S: AsyncSigner> {
    signer: S,
    client: OnlineClient<Config>,
    legacy: LegacyRpcMethods<Config>,
}

impl<S: AsyncSigner> Client<S> {
    pub async fn with_signer(url: &str, account: S) -> Result<Self, Error> {
        let rpc = RpcClient::from_url(url).await?;
        let client = OnlineClient::<Config>::from_rpc_client(rpc.clone()).await?;
        let legacy = LegacyRpcMethods::new(rpc);

        Ok(Self {
            signer: account,
            client,
            legacy,
        })
    }

    async fn get_nonce(&self) -> Result<u64, Error> {
        let best_block = self
            .legacy
            .chain_get_block_hash(None)
            .await?
            .ok_or(Error::Other("Best block not found".into()))?;
        let account_nonce = self
            .client
            .blocks()
            .at(best_block)
            .await?
            .account_nonce(&self.signer.account_id_async().await?)
            .await?;
        Ok(account_nonce)
    }

    pub async fn join(&self, dest: ContractId, data: NodeData) -> Result<(), Error> {
        self.call_auto_weight(
            1000000,
            dest,
            contracts::node_staker::messages::join(data.into()),
        )
        .await
    }

    pub async fn list(&self, addr: ContractId) -> Result<Vec<SerializedNodeData>, Error> {
        self.call_dry(0, addr, contracts::node_staker::messages::list())
            .await
    }

    pub async fn vote(
        &self,
        dest: ContractId,
        me: protocols::contracts::Identity,
        target: protocols::contracts::Identity,
        weight: i32,
    ) -> Result<(), Error> {
        let call = contracts::node_staker::messages::vote(me, target, weight);
        self.call_auto_weight(0, dest, call).await
    }

    pub async fn reclaim(&self, dest: ContractId, me: Identity) -> Result<(), Error> {
        self.call_auto_weight(0, dest, contracts::node_staker::messages::reclaim(me))
            .await
    }

    pub async fn register(
        &self,
        dest: ContractId,
        data: protocols::contracts::UserData,
    ) -> Result<(), Error> {
        self.call_auto_weight(
            0,
            dest,
            contracts::user_manager::messages::register_with_name(
                protocols::username_to_raw(data.name),
                data.to_identity().into(),
            ),
        )
        .await
    }

    pub async fn get_profile_by_name(
        &self,
        dest: ContractId,
        name: UserName,
    ) -> Result<SerializedUserIdentity, Error> {
        let call =
            contracts::user_manager::messages::profile_by_name(protocols::username_to_raw(name));
        self.call_dry(0, dest, call).await
    }

    pub async fn user_exists(&self, dest: ContractId, name: UserName) -> Result<bool, Error> {
        self.get_profile_by_name(dest, name)
            .await
            .map(|p| p.iter().any(|&b| b != 0))
    }

    async fn call_auto_weight<T: parity_scale_codec::Decode>(
        &self,
        value: Balance,
        dest: ContractId,
        call_data: impl InkMessage,
    ) -> Result<T, Error> {
        let (res, weight) = self
            .call_dry_low(value, dest.clone(), call_data.to_bytes())
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

    async fn call_dry<T: parity_scale_codec::Decode>(
        &self,
        value: Balance,
        dest: ContractId,
        call_data: impl InkMessage,
    ) -> Result<T, Error> {
        self.call_dry_low(value, dest, call_data.to_bytes())
            .await
            .map(|(t, ..)| t)
    }

    async fn call_dry_low<T: parity_scale_codec::Decode>(
        &self,
        value: Balance,
        dest: ContractId,
        call_data: Vec<u8>,
    ) -> Result<(T, Weight), Error> {
        self.make_dry_call::<Result<T, ()>>(CallRequest {
            origin: self.signer.account_id_async().await?,
            dest: dest.clone(),
            value,
            gas_limit: None,
            storage_deposit_limit: None,
            input_data: call_data,
        })
        .await
        .and_then(|(e, w)| {
            let e = e.map_err(|_| Error::Other("contract returned `Err`".into()))?;
            Ok((e, w))
        })
    }

    async fn make_dry_call<T: parity_scale_codec::Decode>(
        &self,
        call: CallRequest,
    ) -> Result<(T, Weight), Error> {
        let bytes = call.encode();
        self.legacy
            .state_call("ContractsApi_call", Some(&bytes), None)
            .await
            .and_then(|r| {
                <ContractResult<Result<ExecReturnValue, DispatchError>, Balance, ()>>::decode(
                    &mut r.as_slice(),
                )
                .map_err(|e| Error::Decode(subxt::error::DecodeError::custom(e)))
            })
            .and_then(|r| {
                let res = r
                    .result
                    .map_err(|e| Error::Other(format!("Contract error: {:?}", e)))?;
                let res = T::decode(&mut res.data.as_slice())
                    .map_err(|e| Error::Decode(subxt::error::DecodeError::custom(e)))?;
                Ok((res, r.gas_consumed))
            })
    }

    async fn make_call(&self, call: Payload<Call>) -> Result<(), Error> {
        let nonce = self.get_nonce().await?;

        let tx = self.client.tx();
        tx.validate(&call)?;
        let partial_signed =
            tx.create_partial_signed_with_nonce(&call, nonce, Default::default())?;
        let sp = partial_signed.signer_payload();
        let signature = self.signer.sign_async(&sp.encode()).await?;
        let address = self.signer.account_id_async().await?;
        let signed =
            partial_signed.sign_with_address_and_signature(&address.into(), &signature.into());

        signed
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
