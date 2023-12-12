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

pub struct Storage {
    profiles: HashMap<crate::Identity, Profile>,
    chats: HashMap<crate::ChatName, Chat>,

    // this is true if we are dispatching put_record
    dont_replicate: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            chats: HashMap::new(),
            dont_replicate: false,
        }
    }

    pub fn disable_replication(&mut self) {
        self.dont_replicate = true;
    }

    pub fn enable_replication(&mut self) {
        self.dont_replicate = false;
    }
}

impl libp2p::kad::store::RecordStore for Storage {
    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::ProviderRecord>>
    where
        Self: 'a;
    type RecordsIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::Record>>
    where
        Self: 'a;

    fn get(&self, _: &libp2p::kad::RecordKey) -> Option<std::borrow::Cow<'_, libp2p::kad::Record>> {
        None
    }

    fn put(&mut self, _: libp2p::kad::Record) -> libp2p::kad::store::Result<()> {
        Ok(())
    }

    fn remove(&mut self, _: &libp2p::kad::RecordKey) {}

    fn records(&self) -> Self::RecordsIter<'_> {
        iter::empty()
    }

    fn add_provider(&mut self, _: libp2p::kad::ProviderRecord) -> libp2p::kad::store::Result<()> {
        Ok(())
    }

    fn providers(&self, _: &libp2p::kad::RecordKey) -> Vec<libp2p::kad::ProviderRecord> {
        Vec::new()
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        iter::empty()
    }

    fn remove_provider(&mut self, _: &libp2p::kad::RecordKey, _: &PeerId) {}
}
