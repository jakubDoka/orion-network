use {crypto::TransmutationCircle, primitives::contracts::NodeIdentity};

use {
    crate::Identity,
    component_utils::Codec,
    libp2p::PeerId,
    std::{borrow::Cow, collections::HashMap, iter},
};

mod chat;
mod profile;

pub use {chat::*, profile::*};

fn replicate<'a>(
    kad: &mut libp2p::kad::Behaviour<Storage>,
    key: &impl Codec<'a>,
    value: &impl Codec<'a>,
    meta: crate::RequestMeta,
) {
    if kad.store_mut().replicating {
        return;
    }

    let mut rec = libp2p::kad::Record::new(key.to_bytes(), vec![]);
    meta.encode(&mut rec.value);
    value.encode(&mut rec.value);
    kad.put_record(rec, super::QUORUM)
        .expect("storage to ignore and accept the record");
}

pub struct Storage {
    profiles: HashMap<crate::Identity, FullProfile>,
    chats: HashMap<crate::ChatName, Chat>,
    nodes: HashMap<crate::Identity, NodeIdentity>,

    // this is true if we are dispatching put_record
    replicating: bool,
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
    type RecordsIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::Record>>
    where
        Self: 'a;

    type ProvidedIter<'a> = std::iter::Empty<Cow<'a, libp2p::kad::ProviderRecord>>
    where
        Self: 'a;

    fn get(&self, k: &libp2p::kad::RecordKey) -> Option<std::borrow::Cow<'_, libp2p::kad::Record>> {
        if let Some(id) = Identity::decode(&mut k.as_ref()) {
            if let Some(profile) = self.profiles.get(&id) {
                return Some(Cow::Owned(libp2p::kad::Record::new(
                    k.clone(),
                    FetchProfileResp::from(profile).to_bytes(),
                )));
            }

            if let Some(node) = self.nodes.get(&id) {
                return Some(Cow::Owned(libp2p::kad::Record::new(
                    k.clone(),
                    node.into_bytes().to_vec(),
                )));
            }
        }

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
