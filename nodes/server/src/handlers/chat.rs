use {
    super::*,
    anyhow::ensure,
    chat_logic::*,
    component_utils::{encode_len, Buffer, NoCapOverflow, Reminder},
    core::panic,
    std::{
        collections::{HashMap, VecDeque},
        iter, usize,
    },
};

const MAX_MESSAGE_SIZE: usize = 1024;
const MESSAGE_FETCH_LIMIT: usize = 20;
const BLOCK_SIZE: usize = if cfg!(test) { 1024 * 4 } else { 1024 * 32 };
const BLOCK_HISTORY: usize = 32;

impl SyncHandler for CreateChat {
    fn execute<'a>(
        mut cx: Scope<'a>,
        (name, identity): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        let chat_entry = cx.storage.chats.entry(name);
        crate::ensure!(
            let std::collections::hash_map::Entry::Vacant(entry) = chat_entry,
            CreateChatError::AlreadyExists
        );

        entry.insert(Chat::new(identity));

        Ok(())
    }
}

impl SyncHandler for PerformChatAction {
    fn execute<'a>(
        mut sc: Scope<'a>,
        (name, proof, action): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(proof.verify_chat(name), ChatActionError::InvalidProof);

        let chat = sc
            .storage
            .chats
            .get_mut(&name)
            .ok_or(ChatActionError::ChatNotFound)?;

        let sender_id = crypto::hash::from_raw(&proof.pk);
        let sender = chat
            .members
            .get_mut(&sender_id)
            .ok_or(ChatActionError::NotMember)?;

        crate::ensure!(
            advance_nonce(&mut sender.action, proof.nonce),
            ChatActionError::InvalidAction(sender.action)
        );

        match action {
            ChatAction::AddUser(id) => {
                // TODO: write the member addition to message history so it can be finalized
                crate::ensure!(
                    chat.members.try_insert(id, Member::new()).is_ok(),
                    ChatActionError::AlreadyMember
                );
            }
            ChatAction::SendMessage(Reminder(msg)) => {
                crate::ensure!(
                    msg.len() <= MAX_MESSAGE_SIZE,
                    ChatActionError::MessageTooLarge
                );

                // TODO: move this to context
                let hash_temp = &mut Vec::new();
                let bn = chat.block_number;
                match chat.push_message(Reminder(msg), hash_temp) {
                    Err(Some(hash)) => send_block_proposals(sc.reborrow(), name, bn, hash),
                    Err(None) => return Err(ChatActionError::MessageBlockNotFinalized),
                    Ok(()) => (),
                }

                sc.push(name, ChatEvent::Message(proof, Reminder(msg)));
            }
        }

        Ok(())
    }
}

fn send_block_proposals(sc: Scope, name: ChatName, number: BlockNumber, hash: crypto::Hash) {
    let us = *sc.cx.swarm.local_peer_id();
    let beh = sc.cx.swarm.behaviour_mut();
    let mut msg = [0; std::mem::size_of::<(u8, ChatName, crypto::Hash)>()];
    ProposeMsgBlock::rpc((name, number, hash))
        .encode(&mut msg.as_mut_slice())
        .unwrap();
    for recip in crate::other_replicators_for(&beh.dht.table, name, us) {
        _ = beh.rpc.request(recip, msg);
    }
}

impl SyncHandler for ProposeMsgBlock {
    fn execute<'a>(
        sc: Scope<'a>,
        (chat, number, phash): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        crate::ensure!(let RequestOrigin::Server(origin) = sc.origin, ProposeMsgBlockError::NotServer);

        let index = sc
            .other_replicators_for(chat)
            .map(RequestOrigin::Server)
            .position(|id| id == sc.origin);
        crate::ensure!(let Some(index) = index, ProposeMsgBlockError::NoReplicator);

        crate::ensure!(
            let Some(chat_data) = sc.cx.storage.chats.get_mut(&chat),
            ProposeMsgBlockError::ChatNotFound
        );

        let our_finalized = chat_data.block_number
            - matches!(
                chat_data.stage,
                BlockStage::Unfinalized {
                    proposed: Some(_),
                    ..
                } | BlockStage::Recovering { .. }
            ) as usize;

        match number.cmp(&our_finalized) {
            std::cmp::Ordering::Less if our_finalized - number <= 1 => {
                dbg!("we are ahead, so we can send the block");
                crate::ensure!(
                    let Some(block) = chat_data.finalized.front(),
                    ProposeMsgBlockError::NoBlocks
                );

                if block.hash == phash {
                    return Ok(());
                }

                let packet =
                    SendBlock::rpc((chat, number, Reminder(block.data.as_ref()))).to_bytes();
                _ = sc.cx.swarm.behaviour_mut().rpc.request(origin, packet);
                return Ok(());
            }
            std::cmp::Ordering::Less => todo!("sender needs more complex data recovery"),
            std::cmp::Ordering::Equal => {}
            std::cmp::Ordering::Greater if number - our_finalized <= 1 => {}
            std::cmp::Ordering::Greater => todo!("we are behind, so I guess just wait for blocks"),
        }

        let BlockStage::Unfinalized { proposed, others } = &mut chat_data.stage else {
            return Ok(());
        };

        let we_finalized = proposed.is_some();
        let we_match = proposed.as_ref().map(|p| p.hash) == Some(phash);

        others[index] = phash;

        if others.iter().filter(|h| **h == phash).count()
            > REPLICATION_FACTOR.get() / 2 - we_match as usize
        {
            chat_data.stage = if let Some(block) = proposed.take()
                && block.hash == phash
            {
                if chat_data.finalized.len() == BLOCK_HISTORY {
                    chat_data.finalized.pop_back();
                }
                chat_data.finalized.push_front(block);
                BlockStage::default()
            } else {
                BlockStage::Recovering {
                    final_hash: phash,
                    we_finalized,
                }
            };
        } else if !others.contains(&Default::default()) && we_finalized {
            println!("{:?} {index}", phash);
            for h in others.iter() {
                println!("{:?}", h);
            }
            todo!("no majority, we need to initialize recovery",);
        }

        Ok(())
    }
}

impl SyncHandler for SendBlock {
    fn execute<'a>(
        cx: Scope<'a>,
        (chat, number, Reminder(block)): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        todo!()
    }
}

impl SyncHandler for FetchMessages {
    fn execute<'a>(
        sc: Scope<'a>,
        (chat, mut cursor): Self::Request<'_>,
    ) -> ProtocolResult<'a, Self> {
        let chat = sc
            .cx
            .storage
            .chats
            .get_mut(&chat)
            .ok_or(FetchMessagesError::ChatNotFound)?;

        if cursor == Cursor::INIT {
            cursor.block = chat.block_number;
            cursor.offset = chat.current_block.len();
        }

        let Some(block) = iter::once(chat.current_block.as_mut_slice())
            .chain(chat.stage.unfinalized_block())
            .chain(chat.finalized.iter_mut().map(|b| b.data.as_mut()))
            .nth(chat.block_number - cursor.block)
        else {
            return Ok((Cursor::INIT, Reminder(&[])));
        };

        let message_length = unpack_messages(&mut block[cursor.offset..])
            .take(MESSAGE_FETCH_LIMIT)
            .map(|msg| msg.len() + 2)
            .sum::<usize>();

        let slice = &block[cursor.offset..cursor.offset + message_length];
        cursor.offset += message_length;

        Ok((cursor, Reminder(slice)))
    }
}

#[derive(Codec)]
struct Block {
    hash: crypto::Hash,
    data: Box<[u8]>,
}

#[derive(Codec)]
enum BlockStage {
    Unfinalized {
        proposed: Option<Block>,
        others: [crypto::Hash; REPLICATION_FACTOR.get()],
    },
    Recovering {
        final_hash: crypto::Hash,
        we_finalized: bool,
    },
}

impl Default for BlockStage {
    fn default() -> Self {
        Self::Unfinalized {
            proposed: None,
            others: Default::default(),
        }
    }
}

impl BlockStage {
    fn unfinalized_block(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Unfinalized { proposed, .. } => proposed.as_mut().map(|p| p.data.as_mut()),
            _ => None,
        }
    }
}

#[derive(Codec)]
pub struct Chat {
    members: HashMap<Identity, Member>,
    finalized: VecDeque<Block>,
    current_block: Vec<u8>,
    pub(crate) block_number: BlockNumber,
    stage: BlockStage,
}

impl Chat {
    pub fn new(id: Identity) -> Self {
        Self {
            members: [(id, Member::new())].into(),
            finalized: Default::default(),
            current_block: Vec::with_capacity(BLOCK_SIZE),
            block_number: 0,
            stage: Default::default(),
        }
    }

    pub fn push_message<'a>(
        &mut self,
        msg: impl Codec<'a>,
        hash_temp: &mut Vec<crypto::Hash>,
    ) -> Result<(), Option<crypto::Hash>> {
        let prev_len = self.current_block.len();

        fn try_push<'a>(block: &mut Vec<u8>, msg: impl Codec<'a>) -> Option<()> {
            let len = block.len();
            let buffer = NoCapOverflow::new(block);
            msg.encode(buffer)?;
            let len = buffer.as_mut().len() - len;
            buffer.extend_from_slice(&encode_len(len))
        }

        if try_push(&mut self.current_block, &msg).is_some() {
            return Ok(());
        }

        self.current_block.truncate(prev_len);

        let err = match &mut self.stage {
            BlockStage::Unfinalized { proposed, .. } if proposed.is_some() => return Err(None),
            BlockStage::Unfinalized { proposed, others } => {
                let hash = Self::hash_block(self.current_block.as_slice(), hash_temp);
                if others.iter().filter(|h| **h == hash).count() >= REPLICATION_FACTOR.get() / 2 {
                    self.finalize_current_block(hash);
                } else {
                    *proposed = Some(Block {
                        hash,
                        data: self.current_block.as_slice().into(),
                    });
                    self.current_block.clear();
                    self.block_number += 1;
                }
                Some(hash)
            }
            BlockStage::Recovering { we_finalized, .. } if *we_finalized => return Err(None),
            BlockStage::Recovering {
                final_hash,
                we_finalized,
            } => {
                *we_finalized = true;
                let hash = Self::hash_block(self.current_block.as_slice(), hash_temp);
                if hash == *final_hash {
                    self.finalize_current_block(hash);
                } else {
                    self.current_block.clear();
                }
                Some(hash)
            }
        };

        try_push(&mut self.current_block, msg).expect("we checked size limits");

        Err(err)
    }

    fn finalize_current_block(&mut self, hash: crypto::Hash) {
        self.stage = BlockStage::default();
        self.finalized.push_front(Block {
            hash,
            data: self.current_block.clone().into(),
        });
        self.current_block.clear();
        self.block_number += 1;
    }

    fn hash_block(block: &[u8], hash_temp: &mut Vec<crypto::Hash>) -> crypto::Hash {
        debug_assert!(hash_temp.is_empty());
        unpack_mail(block)
            .map(crypto::hash::from_slice)
            .collect_into(hash_temp);
        hash_temp.sort_unstable();
        hash_temp
            .drain(..)
            .reduce(crypto::hash::combine)
            .unwrap_or_default()
    }
}

#[derive(Codec)]
struct Member {
    action: Nonce,
}

impl Member {
    fn new() -> Self {
        Self { action: 0 }
    }
}

#[derive(Clone, Copy, Codec)]
pub struct Message<'a> {
    pub identiy: Identity,
    pub content: Reminder<'a>,
}

bitflags::bitflags! {
    pub struct Permissions: u8 {
        const MODIFY_PERMISSIONS = 1 << 0;
        const KICK = 1 << 1;
    }
}
