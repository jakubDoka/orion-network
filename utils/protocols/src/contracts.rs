use core::{net::Ipv4Addr, u128};

use crypto::impl_transmute;

use crate::{username_from_raw, RawUserName, UserName};

type Balance = u128;
type Timestamp = u64;
pub type Identity = crypto::sign::SerializedPublicKey;

pub const STAKE_AMOUNT: Balance = 1000000;
pub const INIT_VOTE_POOL: u32 = 3;
pub const STAKE_DURATION_MILIS: Timestamp = 1000 * 60 * 60 * 24 * 30;
pub const BASE_SLASH: Balance = 2;
pub const SLASH_FACTOR: u32 = 1;

#[derive(Debug, Clone)]
pub struct RawUserData {
    pub name: RawUserName,
    pub sign: crypto::sign::SerializedPublicKey,
    pub enc: crypto::enc::SerializedPublicKey,
}

impl TryFrom<RawUserData> for UserData {
    type Error = ();

    fn try_from(RawUserData { name, sign, enc }: RawUserData) -> Result<Self, Self::Error> {
        Ok(UserData {
            name: username_from_raw(name).ok_or(())?,
            sign: sign.into(),
            enc: enc.into(),
        })
    }
}

#[derive(Clone, Copy)]
pub struct UserData {
    pub name: UserName,
    pub sign: crypto::sign::SerializedPublicKey,
    pub enc: crypto::enc::SerializedPublicKey,
}

impl UserData {
    pub fn to_identity(self) -> UserIdentity {
        UserIdentity {
            sign: self.sign,
            enc: self.enc,
        }
    }
}

#[derive(Clone, Copy)]
pub struct UserIdentity {
    pub sign: crypto::sign::SerializedPublicKey,
    pub enc: crypto::enc::SerializedPublicKey,
}

impl UserIdentity {
    pub fn to_data(self, name: UserName) -> UserData {
        UserData {
            name,
            sign: self.sign,
            enc: self.enc,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    pub sign: crypto::sign::SerializedPublicKey,
    pub enc: crypto::enc::SerializedPublicKey,
    pub ip: Ipv4Addr,
    pub port: u16,
}

impl_transmute! {
    NodeData, NODE_DATA_SIZE, SerializedNodeData;
    RawUserData, USER_DATA_SIZE, SerializedUserData;
    UserIdentity, USER_IDENTITY_SIZE, SerializedUserIdentity;
}
