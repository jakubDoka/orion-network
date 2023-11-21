#![cfg_attr(not(feature = "std"), no_std, no_main)]

#[ink::contract]
mod user_manager {
    use protocols::{
        contracts::{SerializedUserIdentity, USER_IDENTITY_SIZE},
        RawUserName,
    };

    #[derive(scale::Decode, scale::Encode)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo))]
    struct Profile(SerializedUserIdentity);

    #[cfg(feature = "std")]
    impl ink::storage::traits::StorageLayout for Profile {
        fn layout(key: &ink::primitives::Key) -> ink::metadata::layout::Layout {
            ink::metadata::layout::Layout::Array(ink::metadata::layout::ArrayLayout::new(
                key,
                USER_IDENTITY_SIZE as u32,
                <u8 as ink::storage::traits::StorageLayout>::layout(&key),
            ))
        }
    }

    #[ink(storage)]
    pub struct UserManager {
        usernames: ink::storage::Mapping<RawUserName, AccountId>,
        has_name: ink::storage::Mapping<AccountId, RawUserName>,
        identities: ink::storage::Mapping<AccountId, Profile>,
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
        pub fn register_with_name(&mut self, name: RawUserName, data: SerializedUserIdentity) {
            self.register(data);
            self.pick_name(name);
        }

        #[ink(message)]
        pub fn register(&mut self, data: SerializedUserIdentity) {
            self.identities.insert(Self::env().caller(), &Profile(data));
        }

        #[ink(message)]
        pub fn pick_name(&mut self, name: RawUserName) {
            assert!(self.has_name.insert(&Self::env().caller(), &name).is_none());
            assert!(self.usernames.insert(name, &Self::env().caller()).is_none());
        }

        #[ink(message)]
        pub fn give_up_name(&mut self, name: RawUserName) {
            assert_eq!(self.usernames.take(&name), Some(Self::env().caller()));
            assert_eq!(self.has_name.take(&Self::env().caller()), Some(name));
        }

        #[ink(message)]
        pub fn transfere_name(&mut self, name: RawUserName, target: AccountId) {
            self.give_up_name(name);
            assert!(self.has_name.insert(&target, &name).is_none());
            assert!(self.usernames.insert(name, &target).is_none());
        }

        #[ink(message)]
        pub fn get_profile(&self, account: AccountId) -> SerializedUserIdentity {
            self.identities
                .get(&account)
                .map_or([0; USER_IDENTITY_SIZE], |profile| profile.0)
        }

        #[ink(message)]
        pub fn get_profile_by_name(&self, name: RawUserName) -> SerializedUserIdentity {
            self.usernames
                .get(&name)
                .and_then(|account| self.identities.get(account))
                .map_or([0; USER_IDENTITY_SIZE], |profile| profile.0)
        }
    }
}
