#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod user_manager {
    use {
        core::marker::PhantomData,
        crypto::{Serialized, TransmutationCircle},
        primitives::{contracts::StoredUserIdentity, RawUserName},
    };

    #[derive(scale::Decode, scale::Encode)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    struct Profile {
        sign: crypto::AnyHash,
        enc: crypto::AnyHash,
    }

    impl Profile {
        fn from_bytes(bytes: Serialized<StoredUserIdentity>) -> Self {
            let data = StoredUserIdentity::from_bytes(bytes);
            Self {
                sign: data.sign.0,
                enc: data.enc.0,
            }
        }

        fn to_bytes(&self) -> Serialized<StoredUserIdentity> {
            StoredUserIdentity {
                sign: (self.sign, PhantomData),
                enc: (self.enc, PhantomData),
            }
            .into_bytes()
        }
    }

    #[ink(storage)]
    pub struct UserManager {
        username_to_owner: ink::storage::Mapping<RawUserName, AccountId>,
        owner_to_username: ink::storage::Mapping<AccountId, RawUserName>,
        identity_to_username: ink::storage::Mapping<crypto::AnyHash, RawUserName>,
        identities: ink::storage::Mapping<AccountId, Profile>,
    }

    impl Default for UserManager {
        fn default() -> Self {
            Self::new()
        }
    }

    impl UserManager {
        #[ink(constructor)]
        pub fn new() -> Self {
            Self {
                username_to_owner: ink::storage::Mapping::new(),
                identities: ink::storage::Mapping::new(),
                identity_to_username: ink::storage::Mapping::new(),
                owner_to_username: ink::storage::Mapping::new(),
            }
        }

        #[ink(message)]
        pub fn register_with_name(
            &mut self,
            name: RawUserName,
            data: Serialized<StoredUserIdentity>,
        ) {
            self.register(data);
            self.pick_name(name);
        }

        #[ink(message)]
        pub fn register(&mut self, data: Serialized<StoredUserIdentity>) {
            self.identities
                .insert(Self::env().caller(), &Profile::from_bytes(data));
        }

        #[ink(message)]
        pub fn pick_name(&mut self, name: RawUserName) {
            assert!(self
                .owner_to_username
                .insert(Self::env().caller(), &name)
                .is_none());
            assert!(self
                .username_to_owner
                .insert(name, &Self::env().caller())
                .is_none());
            assert!(self
                .identity_to_username
                .insert(
                    self.identities
                        .get(Self::env().caller())
                        .expect("caller to have identity")
                        .sign,
                    &name
                )
                .is_none());
        }

        #[ink(message)]
        pub fn give_up_name(&mut self, name: RawUserName) {
            assert_eq!(
                self.username_to_owner.take(name),
                Some(Self::env().caller())
            );
            assert_eq!(
                self.owner_to_username.take(Self::env().caller()),
                Some(name)
            );
            assert_eq!(
                self.identity_to_username.take(
                    self.identities
                        .get(Self::env().caller())
                        .expect("caller to have identity")
                        .sign
                ),
                Some(name)
            );
        }

        #[ink(message)]
        pub fn transfere_name(&mut self, name: RawUserName, target: AccountId) {
            self.give_up_name(name);
            assert!(self.owner_to_username.insert(target, &name).is_none());
            assert!(self.username_to_owner.insert(name, &target).is_none());
            let identity = self
                .identities
                .get(target)
                .expect("target to have identity");
            assert!(self
                .identity_to_username
                .insert(identity.sign, &name)
                .is_none());
        }

        #[ink(message)]
        pub fn get_profile(&self, account: AccountId) -> Option<Serialized<StoredUserIdentity>> {
            self.identities
                .get(account)
                .map(|profile| profile.to_bytes())
        }

        #[ink(message)]
        pub fn get_profile_by_name(
            &self,
            name: RawUserName,
        ) -> Option<Serialized<StoredUserIdentity>> {
            self.username_to_owner
                .get(name)
                .and_then(|account| self.identities.get(account))
                .map(|profile| profile.to_bytes())
        }

        #[ink(message)]
        pub fn get_username(&self, identity: crypto::AnyHash) -> Option<RawUserName> {
            self.identity_to_username.get(identity)
        }
    }
}
