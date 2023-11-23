#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod node_staker {
    use core::u32;
    use ink::prelude::vec::Vec;
    use protocols::contracts::*;

    #[derive(scale::Decode, scale::Encode)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    struct Votes {
        pool: u32,
        rating: u32,
    }

    impl Default for Votes {
        fn default() -> Self {
            Self {
                pool: INIT_VOTE_POOL,
                rating: 0,
            }
        }
    }

    #[derive(scale::Decode, scale::Encode)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo))]
    struct EncKey(SerializedNodeData);

    #[cfg(feature = "std")]
    impl ink::storage::traits::StorageLayout for EncKey {
        fn layout(key: &ink::primitives::Key) -> ink::metadata::layout::Layout {
            ink::metadata::layout::Layout::Array(ink::metadata::layout::ArrayLayout::new(
                key,
                crypto::enc::PUBLIC_KEY_SIZE as u32,
                <u8 as ink::storage::traits::StorageLayout>::layout(&key),
            ))
        }
    }

    #[derive(scale::Decode, scale::Encode)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    struct Stake {
        owner: AccountId,
        amount: Balance,
        created_at: Timestamp,
        votes: Votes,
        enc_key: EncKey,
    }

    impl Stake {
        fn apply_slashes(&self) -> u128 {
            let mut amount = self.amount;

            if self.votes.rating > 0 {
                amount = self
                    .amount
                    .saturating_sub(BASE_SLASH << (self.votes.rating * SLASH_FACTOR) as Balance);
            }

            amount
        }
    }

    /// Defines the storage of your contract.
    /// Add new fields to the below struct in order
    /// to add new static storage fields to your contract.
    #[ink(storage)]
    pub struct NodeStaker {
        stakes: ink::storage::Mapping<EdIdentity, Stake>,
        stake_list: Vec<EdIdentity>,
    }

    impl NodeStaker {
        /// Constructor that initializes the `bool` value to the given `init_value`.
        #[ink(constructor)]
        pub fn new() -> Self {
            Self {
                stakes: ink::storage::Mapping::new(),
                stake_list: Vec::new(),
            }
        }

        #[ink(message, payable)]
        pub fn join(&mut self, identity: SerializedNodeData) {
            let amount = self.env().transferred_value();
            assert!(amount == STAKE_AMOUNT, "wrong amount");
            let identity = NodeData::from(identity);
            let stake = Stake {
                amount,
                owner: Self::env().caller(),
                created_at: Self::env().block_timestamp(),
                votes: Votes::default(),
                enc_key: EncKey(identity.into()),
            };
            assert!(
                self.stakes
                    .insert(crypto::sign::PublicKey::from(identity.sign).ed, &stake)
                    .is_none(),
                "already joined"
            );
            self.stake_list
                .push(crypto::sign::PublicKey::from(identity.sign).ed);
        }

        #[ink(message)]
        pub fn vote(&mut self, identity: EdIdentity, target: EdIdentity, rating: i32) {
            let mut stake = self.stakes.get(&identity).expect("no stake to wote with");
            assert!(stake.owner == self.env().caller(), "not owner");
            let mut target_stake = self.stakes.get(&target).expect("target does not exist");
            assert!(
                target_stake.owner != Self::env().caller(),
                "cannot vote for self"
            );
            stake.votes.pool = stake
                .votes
                .pool
                .checked_sub(rating.unsigned_abs())
                .expect("not enough votes in pool");
            target_stake.votes.rating = target_stake
                .votes
                .rating
                .checked_add_signed(-rating)
                .expect("too many votes casted");
            self.stakes.insert(identity, &stake);
            self.stakes.insert(target, &target_stake);
        }

        #[ink(message)]
        pub fn list(&self) -> Vec<EdIdentity> {
            self.stake_list.clone()
        }

        #[ink(message)]
        pub fn get_by_identity(&self, identity: EdIdentity) -> SerializedNodeData {
            self.stakes.get(&identity).expect("not joined").enc_key.0
        }

        #[ink(message)]
        pub fn reclaim(&mut self, identity: EdIdentity) {
            let stake = self.stakes.get(&identity).expect("not joined");
            assert!(stake.owner == self.env().caller(), "not owner");
            assert!(
                stake.created_at + STAKE_DURATION_MILIS <= self.env().block_timestamp(),
                "still locked"
            );
            self.stakes.remove(&identity);
            ink::env::debug_println!("current balance: {}", self.env().balance());
            self.env()
                .transfer(Self::env().caller(), stake.apply_slashes())
                .expect("transfer failed");

            self.stake_list
                .iter()
                .position(|&x| x == identity)
                .map(|i| self.stake_list.swap_remove(i));
        }
    }

    #[cfg(test)]
    mod tests {
        use std::net::Ipv4Addr;

        use super::*;
        use ink::{env::test as ink_env, env::DefaultEnvironment as Env, primitives::AccountId};

        fn init_contract() -> NodeStaker {
            ink_env::set_callee::<Env>(ink_env::default_accounts::<Env>().charlie);
            NodeStaker::new()
        }

        fn accounts() -> [AccountId; 2] {
            let accounts = ink_env::default_accounts::<Env>();
            [accounts.alice, accounts.bob]
        }

        fn identities() -> [EdIdentity; 2] {
            [[0x01; 32], [0x02; 32]]
        }

        fn join(staker: &mut NodeStaker, amount: Balance, identity: EdIdentity, to: AccountId) {
            ink_env::set_caller::<Env>(to);
            ink_env::set_value_transferred::<Env>(amount);
            ink_env::set_block_timestamp::<Env>(0);
            staker.join(
                NodeData {
                    sign: crypto::sign::PublicKey {
                        ed: identity,
                        dili: [0x01; crypto::sign::PUBLIC_KEY_SIZE - 32],
                    }
                    .into(),
                    enc: [0x01; crypto::enc::PUBLIC_KEY_SIZE].into(),
                    ip: Ipv4Addr::new(127, 0, 0, 1),
                    port: 8080,
                }
                .into(),
            );
            ink_env::set_account_balance::<Env>(
                ink_env::callee::<Env>(),
                ink_env::get_account_balance::<Env>(ink_env::callee::<Env>()).unwrap() + amount,
            )
        }

        fn vote(
            staker: &mut NodeStaker,
            identity: EdIdentity,
            target: EdIdentity,
            rating: i32,
            to: AccountId,
        ) {
            ink_env::set_caller::<Env>(to);
            staker.vote(identity, target, rating);
        }

        fn reclaim(
            staker: &mut NodeStaker,
            identity: EdIdentity,
            block_timestamp: Timestamp,
            to: AccountId,
        ) {
            ink_env::set_caller::<Env>(to);
            ink_env::set_block_timestamp::<Env>(block_timestamp);
            staker.reclaim(identity);
        }

        #[ink::test]
        fn tjoin() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            assert_eq!(node_staker.stakes.get(&identity).unwrap().owner, alice);
        }

        #[ink::test]
        #[should_panic(expected = "already joined")]
        fn double_join() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
        }

        #[ink::test]
        #[should_panic(expected = "wrong amount")]
        fn join_wrong_amount() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            join(&mut node_staker, STAKE_AMOUNT + 1, identity, alice);
        }

        #[ink::test]
        fn tvote() {
            let mut node_staker = init_contract();
            let [identity, target] = identities();
            let [alice, bob] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            join(&mut node_staker, STAKE_AMOUNT, target, bob);
            vote(&mut node_staker, identity, target, -1, alice);
            assert_eq!(
                node_staker.stakes.get(&identity).unwrap().votes.pool,
                INIT_VOTE_POOL - 1
            );
            assert_eq!(node_staker.stakes.get(&target).unwrap().votes.rating, 1);
        }

        #[ink::test]
        #[should_panic(expected = "not enough votes in pool")]
        fn vote_not_enough_votes() {
            let mut node_staker = init_contract();
            let [identity, target] = identities();
            let [alice, bob] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            join(&mut node_staker, STAKE_AMOUNT, target, bob);
            vote(
                &mut node_staker,
                identity,
                target,
                -(INIT_VOTE_POOL as i32 + 1),
                alice,
            );
        }

        #[ink::test]
        #[should_panic(expected = "too many votes casted")]
        fn vote_too_many_votes() {
            let mut node_staker = init_contract();
            let [identity, target] = identities();
            let [alice, bob] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            join(&mut node_staker, STAKE_AMOUNT, target, bob);
            vote(&mut node_staker, identity, target, 1, alice);
        }

        #[ink::test]
        fn treclaim() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            reclaim(&mut node_staker, identity, STAKE_DURATION_MILIS, alice);
            assert!(node_staker.stakes.get(&identity).is_none());
        }

        #[ink::test]
        #[should_panic(expected = "not joined")]
        fn reclaim_not_joined() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            reclaim(&mut node_staker, identity, STAKE_DURATION_MILIS, alice);
        }

        #[ink::test]
        #[should_panic(expected = "not owner")]
        fn reclaim_not_owner() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, bob] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            reclaim(&mut node_staker, identity, STAKE_DURATION_MILIS, bob);
        }

        #[ink::test]
        #[should_panic(expected = "still locked")]
        fn reclaim_transfer_failed() {
            let mut node_staker = init_contract();
            let [identity, ..] = identities();
            let [alice, ..] = accounts();
            join(&mut node_staker, STAKE_AMOUNT, identity, alice);
            reclaim(&mut node_staker, identity, STAKE_DURATION_MILIS - 1, alice);
        }
    }

    #[cfg(all(test, feature = "e2e-tests"))]
    mod e2e_tests {
        use super::*;
        use ink_e2e::build_message;

        type E2EResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

        #[ink_e2e::test]
        async fn call_runtime_works(mut client: ink_e2e::Client<C, E>) -> E2EResult<()> {
            // given
            let constructor = NodeStakerRef::new();
            let contract_acc_id = client
                .instantiate("node_staker", &ink_e2e::alice(), constructor, 0, None)
                .await
                .expect("instantiate failed")
                .account_id;

            let get_balance = build_message::<NodeStakerRef>(contract_acc_id.clone())
                .call(|contract| contract.join([0x01; 32]));
            let res = client
                .call_dry_run(&ink_e2e::alice(), &get_balance, STAKE_AMOUNT, None)
                .await;
            dbg!(&res.debug_message());
            res.return_value();

            Ok(())
        }
    }
}
