use {
    crate::{username_from_raw, RawUserName, UserName},
    core::u128,
    crypto::{enc, impl_transmute, sign},
};

type Balance = u128;
type Timestamp = u64;

pub const STAKE_AMOUNT: Balance = 1000000;
pub const INIT_VOTE_POOL: u32 = 3;
pub const STAKE_DURATION_MILIS: Timestamp = 1000 * 60 * 60 * 24 * 30;
pub const BASE_SLASH: Balance = 2;
pub const SLASH_FACTOR: u32 = 1;

#[derive(Debug, Clone)]
pub struct RawUserData {
    pub name: RawUserName,
    pub sign: sign::PublicKey,
    pub enc: enc::PublicKey,
}

impl TryFrom<RawUserData> for UserData {
    type Error = ();

    fn try_from(RawUserData { name, sign, enc }: RawUserData) -> Result<Self, Self::Error> {
        Ok(UserData {
            name: username_from_raw(name).ok_or(())?,
            sign,
            enc,
        })
    }
}

#[derive(Clone, Copy)]
pub struct UserData {
    pub name: UserName,
    pub sign: sign::PublicKey,
    pub enc: enc::PublicKey,
}

impl UserData {
    pub fn to_identity(self) -> UserIdentity {
        UserIdentity {
            sign: self.sign,
            enc: self.enc,
        }
    }

    pub fn to_stored(self) -> StoredUserData {
        StoredUserData {
            name: self.name,
            sign: crypto::hash::new(&self.sign),
            enc: crypto::hash::new(&self.enc),
        }
    }
}

#[derive(Clone, Copy)]
pub struct StoredUserData {
    pub name: UserName,
    pub sign: crypto::Hash<sign::PublicKey>,
    pub enc: crypto::Hash<enc::PublicKey>,
}

#[derive(Clone, Copy)]
pub struct UserIdentity {
    pub sign: sign::PublicKey,
    pub enc: enc::PublicKey,
}

impl UserIdentity {
    pub fn to_data(self, name: UserName) -> UserData {
        UserData {
            name,
            sign: self.sign,
            enc: self.enc,
        }
    }

    pub fn to_stored(self) -> StoredUserIdentity {
        StoredUserIdentity {
            sign: crypto::hash::new(&self.sign),
            enc: crypto::hash::new(&self.enc),
        }
    }
}

#[derive(Clone, Copy)]
pub struct StoredUserIdentity {
    pub sign: crypto::Hash<sign::PublicKey>,
    pub enc: crypto::Hash<enc::PublicKey>,
}

impl StoredUserIdentity {
    pub fn verify(&self, identity: &UserIdentity) -> bool {
        crypto::hash::verify(&identity.sign, self.sign)
            && crypto::hash::verify(&identity.enc, self.enc)
    }

    pub fn to_data(self, name: UserName) -> StoredUserData {
        StoredUserData {
            name,
            sign: self.sign,
            enc: self.enc,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    pub sign: sign::PublicKey,
    pub enc: enc::PublicKey,
}

impl NodeData {
    pub fn to_stored(self) -> StoredNodeData {
        StoredNodeData {
            sign: crypto::hash::new(&self.sign),
            enc: crypto::hash::new(&self.enc),
            id: self.sign.ed,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StoredNodeData {
    pub sign: crypto::Hash<sign::PublicKey>,
    pub enc: crypto::Hash<enc::PublicKey>,
    pub id: sign::Ed,
}

#[derive(Debug, Clone, Copy)]
pub struct NodeIdentity {
    pub sign: sign::PublicKey,
    pub enc: enc::PublicKey,
}

impl NodeIdentity {
    pub fn to_stored(self) -> StoredNodeIdentity {
        StoredNodeIdentity {
            sign: crypto::hash::new(&self.sign),
            enc: crypto::hash::new(&self.enc),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StoredNodeIdentity {
    pub sign: crypto::Hash<sign::PublicKey>,
    pub enc: crypto::Hash<enc::PublicKey>,
}

impl_transmute! {
    NodeData,
    RawUserData,
    UserIdentity,
    NodeIdentity,
    StoredUserData,
    StoredNodeData,
    StoredUserIdentity,
    StoredNodeIdentity,
}
