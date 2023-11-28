#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod user_manager {
    use core::marker::PhantomData;

    use {
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
        usernames: ink::storage::Mapping<RawUserName, AccountId>,
        has_name: ink::storage::Mapping<AccountId, RawUserName>,
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
                usernames: ink::storage::Mapping::new(),
                identities: ink::storage::Mapping::new(),
                has_name: ink::storage::Mapping::new(),
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
            assert!(self.has_name.insert(Self::env().caller(), &name).is_none());
            assert!(self.usernames.insert(name, &Self::env().caller()).is_none());
        }

        #[ink(message)]
        pub fn give_up_name(&mut self, name: RawUserName) {
            assert_eq!(self.usernames.take(name), Some(Self::env().caller()));
            assert_eq!(self.has_name.take(Self::env().caller()), Some(name));
        }

        #[ink(message)]
        pub fn transfere_name(&mut self, name: RawUserName, target: AccountId) {
            self.give_up_name(name);
            assert!(self.has_name.insert(target, &name).is_none());
            assert!(self.usernames.insert(name, &target).is_none());
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
            self.usernames
                .get(name)
                .and_then(|account| self.identities.get(account))
                .map(|profile| profile.to_bytes())
        }
    }
}
