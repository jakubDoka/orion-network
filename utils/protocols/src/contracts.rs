use core::{net::Ipv4Addr, str::FromStr, u128};

use crypto::impl_transmute;

use crate::{UserName, USER_NAME_CAP};

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
    name: [u8; USER_NAME_CAP],
    sign: crypto::sign::SerializedPublicKey,
    enc: crypto::enc::SerializedPublicKey,
}

impl TryFrom<RawUserData> for UserData {
    type Error = ();

    fn try_from(RawUserData { name, sign, enc }: RawUserData) -> Result<Self, Self::Error> {
        let len = name.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
        let name = &name[..len];
        Ok(UserData {
            name: UserName::from_str(core::str::from_utf8(&name).map_err(|_| ())?)
                .map_err(|_| ())?,
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

#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    pub sign: crypto::sign::SerializedPublicKey,
    pub enc: crypto::enc::SerializedPublicKey,
    pub ip: Ipv4Addr,
    pub port: u16,
}

impl_transmute! {
    NodeData, NODE_DATA_SIZE, SerializedNodeData;
    UserData, USER_DATA_SIZE, SerializedUserData;
}
