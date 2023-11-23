#![feature(lazy_cell)]
use std::str::FromStr;
use std::u64;

use parity_scale_codec::{Decode, Encode as _};
use polkadot::contracts::calls::types::Call;
use polkadot::runtime_types::pallet_contracts_primitives::{ContractResult, ExecReturnValue};
use polkadot::runtime_types::sp_runtime::DispatchError;
use polkadot::runtime_types::sp_weights::weight_v2::Weight;
use protocols::contracts::{EdIdentity, SerializedUserIdentity};
use protocols::contracts::{NodeData, SerializedNodeData};
use protocols::UserName;
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::RpcClient;
use subxt::tx::{Payload, Signer};
use subxt::{OnlineClient, PolkadotConfig};
use subxt_signer::bip39::Mnemonic;

pub use serde_json::json;
pub use subxt::tx::TxPayload;

pub type Config = PolkadotConfig;
pub type Balance = u128;
pub type ContractId = <Config as subxt::Config>::AccountId;
pub type AccountId = <Config as subxt::Config>::AccountId;
pub type Error = subxt::Error;
pub type Keypair = subxt_signer::sr25519::Keypair;
pub type Signature = subxt_signer::sr25519::Signature;
pub type CallPayload = Payload<Call>;

pub fn immortal_era() -> String {
    encode_then_hex(&subxt::utils::Era::Immortal)
}

pub fn to_hex(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes.as_ref()))
}

pub fn encode_then_hex<E: parity_scale_codec::Encode>(input: &E) -> String {
    format!("0x{}", hex::encode(input.encode()))
}

pub fn encode_tip(value: u128) -> String {
    encode_then_hex(&parity_scale_codec::Compact(value))
}

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
pub trait TransactionHandler {
    async fn account_id_async(&self) -> Result<AccountId, Error>;
    async fn handle(&self, client: &InnerClient, call: impl TxPayload) -> Result<(), Error>;
}

impl TransactionHandler for Keypair {
    async fn account_id_async(&self) -> Result<AccountId, Error> {
        Ok(self.public_key().into())
    }

    async fn handle(&self, inner: &InnerClient, call: impl TxPayload) -> Result<(), Error> {
        let nonce = inner.get_nonce(&Signer::<Config>::account_id(self)).await?;

        inner
            .client
            .tx()
            .create_signed_with_nonce(&call, self, nonce, Default::default())?
            .submit_and_watch()
            .await?
            .wait_for_in_block()
            .await?
            .wait_for_success()
            .await
            .map(drop)
    }
}

pub struct InnerClient {
    pub client: OnlineClient<Config>,
    pub legacy: LegacyRpcMethods<Config>,
}

impl InnerClient {
    pub async fn get_nonce(&self, account: &AccountId) -> Result<u64, Error> {
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
            .account_nonce(account)
            .await?;
        Ok(account_nonce)
    }
}

pub struct Client<S: TransactionHandler> {
    signer: S,
    inner: InnerClient,
}

impl<S: TransactionHandler> Client<S> {
    pub async fn with_signer(url: &str, account: S) -> Result<Self, Error> {
        let rpc = RpcClient::from_url(url).await?;
        let client = OnlineClient::<Config>::from_rpc_client(rpc.clone()).await?;
        let legacy = LegacyRpcMethods::new(rpc);

        Ok(Self {
            signer: account,

            inner: InnerClient { client, legacy },
        })
    }

    pub async fn join(&self, dest: ContractId, data: NodeData) -> Result<(), Error> {
        self.call_auto_weight(
            1000000,
            dest,
            contracts::node_staker::messages::join(data.into()),
        )
        .await
    }

    pub async fn list(&self, addr: ContractId) -> Result<Vec<EdIdentity>, Error> {
        self.call_dry(0, addr, contracts::node_staker::messages::list())
            .await
    }

    pub async fn get_by_identity(
        &self,
        addr: ContractId,
        id: protocols::contracts::EdIdentity,
    ) -> Result<SerializedNodeData, Error> {
        self.call_dry(
            0,
            addr,
            contracts::node_staker::messages::get_by_identity(id),
        )
        .await
    }

    pub async fn vote(
        &self,
        dest: ContractId,
        me: protocols::contracts::EdIdentity,
        target: protocols::contracts::EdIdentity,
        weight: i32,
    ) -> Result<(), Error> {
        let call = contracts::node_staker::messages::vote(me, target, weight);
        self.call_auto_weight(0, dest, call).await
    }

    pub async fn reclaim(
        &self,
        dest: ContractId,
        me: protocols::contracts::EdIdentity,
    ) -> Result<(), Error> {
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
        let call = contracts::user_manager::messages::get_profile_by_name(
            protocols::username_to_raw(name),
        );
        self.call_dry(0, dest, call)
            .await
            .and_then(|r: SerializedUserIdentity| {
                if r.iter().any(|&b| b != 0) {
                    Ok(r)
                } else {
                    Err(Error::Other("user not found".into()))
                }
            })
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
        let (res, mut weight) = self
            .call_dry_low(value, dest.clone(), call_data.to_bytes())
            .await?;

        weight.ref_time = weight.ref_time * 10;
        weight.proof_size = weight.proof_size * 10;

        self.signer
            .handle(
                &self.inner,
                polkadot::tx().contracts().call(
                    dest.into(),
                    value,
                    weight,
                    None,
                    call_data.to_bytes(),
                ),
            )
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
        let (e, w) = self
            .make_dry_call::<Result<T, ()>>(CallRequest {
                origin: self.signer.account_id_async().await?,
                dest: dest.clone(),
                value,
                gas_limit: None,
                storage_deposit_limit: None,
                input_data: call_data,
            })
            .await?;
        let e = e.map_err(|_| Error::Other("contract returned `Err`".into()))?;
        Ok((e, w))
    }

    async fn make_dry_call<T: parity_scale_codec::Decode>(
        &self,
        call: CallRequest,
    ) -> Result<(T, Weight), Error> {
        let bytes = call.encode();
        let r = self
            .inner
            .legacy
            .state_call("ContractsApi_call", Some(&bytes), None)
            .await?;

        let r = <ContractResult<Result<ExecReturnValue, DispatchError>, Balance, ()>>::decode(
            &mut r.as_slice(),
        )
        .map_err(|e| Error::Decode(subxt::error::DecodeError::custom(e)))?;

        let res = r.result.map_err(|e| match e {
            DispatchError::Module(me) => {
                let meta = self.inner.client.metadata();
                let pallet = meta.pallet_by_index(me.index).unwrap();
                let error = pallet.error_variant_by_index(me.error[0]).unwrap();
                Error::Other(format!("dispatch error: {}.{}", pallet.name(), error.name))
            }
            e => Error::Other(format!("dispatch error: {:?}", e)),
        })?;
        let res = T::decode(&mut res.data.as_slice())
            .map_err(|e| Error::Decode(subxt::error::DecodeError::custom(e)))?;
        Ok((res, r.gas_consumed))
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
