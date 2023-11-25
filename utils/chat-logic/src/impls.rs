use component_utils::arrayvec::ArrayString;
use libp2p::swarm::NetworkBehaviour;

pub const CHAT_NAME_CAP: usize = 32;

pub type ChatName = ArrayString<CHAT_NAME_CAP>;

mod search_peers;
mod storage;

pub use search_peers::{Chat, Profile, SearchPeers};
pub use storage::KadStorage;

compose_handlers! {
    Server {
        sp: SearchPeers<Profile>,
        sc: SearchPeers<Chat>,
    }
}
