use {
    libp2p::PeerId,
    std::{borrow::Cow, collections::HashMap, iter},
};

mod chat;
mod profile;
mod replication;

pub use {chat::*, profile::*, replication::*};

macro_rules! replicating_handlers {
    ($($mod:ident::{$($ty:ident),* $(,)*}),* $(,)*) =>
        {$( $( pub type $ty = Replicated<$mod::$ty>; )* )*};
}

replicating_handlers! {
    // TODO: replicating some of the requests is suboptimal
    profile::{CreateAccount, SetVault, SendMail, ReadMail, FetchProfile},
    chat::{CreateChat, AddUser, SendMessage},
}
