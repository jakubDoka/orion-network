use {
    crate::{Dispatches, Handler, Identity, Proof, Server},
    component_utils::{Codec, Reminder},
    crypto::TransmutationCircle,
    libp2p::PeerId,
    primitives::contracts::NodeIdentity,
    rpc::CallId,
    std::{borrow::Cow, collections::HashMap, iter},
};

mod chat;
mod profile;

pub use {chat::*, profile::*};

fn make_new_replication_record<H, S>(key: &H::Topic, value: &H::Request<'_>) -> libp2p::kad::Record
where
    H: Handler<Context = libp2p::kad::Behaviour<Storage>>,
    S: Dispatches<H>,
{
    make_replication_record::<H>(key, value, (S::PREFIX, CallId::whatever()))
}

fn make_replication_record<H: Handler<Context = libp2p::kad::Behaviour<Storage>>>(
    key: &H::Topic,
    value: &H::Request<'_>,
    meta: crate::RequestMeta,
) -> libp2p::kad::Record {
    let mut rec = libp2p::kad::Record::new(key.to_bytes(), vec![]);
    meta.encode(&mut rec.value);
    value.encode(&mut rec.value);
    rec
}

fn replicate<H: Handler<Context = libp2p::kad::Behaviour<Storage>>>(
    kad: &mut H::Context,
    key: &H::Topic,
    value: &H::Request<'_>,
    meta: crate::RequestMeta,
) {
    if kad.store_mut().replicating {
        return;
    }

    let rec = make_replication_record::<H>(key, value, meta);
    kad.put_record(rec, super::QUORUM)
        .expect("storage to ignore and accept the record");
}

pub struct Storage {
    profiles: HashMap<crate::Identity, Profile>,
    chats: HashMap<crate::ChatName, Chat>,
    nodes: HashMap<crate::Identity, NodeIdentity>,

    // this is true if we are dispatching put_record
    replicating: bool,
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
            nodes: HashMap::new(),
            replicating: false,
        }
    }

    pub fn start_replication(&mut self) {
        self.replicating = true;
    }

    pub fn stop_replication(&mut self) {
        self.replicating = false;
    }
}

impl libp2p::kad::store::RecordStore for Storage {
    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::ProviderRecord>>
    where
        Self: 'a;
    type RecordsIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::Record>>
    where
        Self: 'a;

    fn get(&self, k: &libp2p::kad::RecordKey) -> Option<std::borrow::Cow<'_, libp2p::kad::Record>> {
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
