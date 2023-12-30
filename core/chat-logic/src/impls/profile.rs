use {
    crate::{Identity, Nonce, Proof, Topic},
    component_utils::Reminder,
    crypto::{enc, sign, Serialized},
};

pub const MAIL_BOX_CAP: usize = 1024 * 1024;

component_utils::protocol! {'a:
    #[derive(Clone)]
    struct Profile {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        last_sig: Serialized<sign::Signature>,
        vault_version: Nonce,
        mail_action: Nonce,
        vault: Vec<u8>,
        mail: Vec<u8>,
    }

    #[derive(Clone, Copy)]
    struct BorrowedProfile<'a> {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        last_sig: Serialized<sign::Signature>,
        vault_version: Nonce,
        mail_action: Nonce,
        vault: &'a [u8],
        mail: &'a [u8],
    }
}

impl Profile {
    pub fn read_mail(&mut self) -> &[u8] {
        // SAFETY: thre resulting slice locks mutable access to self, we just need to truncate
        // while preserving the borrow
        let slice = unsafe { std::mem::transmute(self.mail.as_slice()) };
        // SAFETY: while the slice exists we cannot push to `self.mail` thus truncating is safe, we
        // avoid truncate since it calls destructors witch requires mutable access to slice memory,
        // we dont want that
        unsafe { self.mail.set_len(0) };
        slice
    }

    pub fn push_mail(&mut self, content: &[u8]) {
        self.mail.extend((content.len() as u16).to_be_bytes());
        self.mail.extend_from_slice(content);
    }
}

impl<'a> From<&'a Profile> for BorrowedProfile<'a> {
    fn from(profile: &'a Profile) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
            last_sig: profile.last_sig,
            vault_version: profile.vault_version,
            mail_action: profile.mail_action,
            vault: profile.vault.as_slice(),
            mail: profile.mail.as_slice(),
        }
    }
}

impl<'a> BorrowedProfile<'a> {
    pub fn is_valid(&self) -> bool {
        Proof {
            pk: self.sign,
            signature: self.last_sig,
            nonce: self.vault_version,
        }
        .verify_vault(self.vault)
    }
}

impl<'a> From<BorrowedProfile<'a>> for Profile {
    fn from(profile: BorrowedProfile<'a>) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
            last_sig: profile.last_sig,
            vault_version: profile.vault_version,
            mail_action: profile.mail_action,
            vault: profile.vault.to_vec(),
            mail: profile.mail.to_vec(),
        }
    }
}

impl From<&Profile> for FetchProfileResp {
    fn from(profile: &Profile) -> Self {
        Self {
            sign: profile.sign,
            enc: profile.enc,
        }
    }
}

impl Topic for Identity {
    type Event<'a> = ProfileEvent<'a>;
    type Record = Profile;
}

type ProfileEvent<'a> = Reminder<'a>;

component_utils::protocol! {'a:
    struct FetchProfileResp {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
    }
}

component_utils::gen_simple_error! {
    enum FetchProfileError {
        NotFound => "account not found",
    }

    enum CreateAccountError {
        InvalidProof => "invalid proof",
        AlreadyExists => "account already exists",
    }

    enum SetVaultError {
        NotFound => "account not found",
        InvalidProof => "invalid proof",
        InvalidAction => "invalid action",
    }

    enum FetchVaultError {
        NotFound => "account not found",
    }

    enum ReadMailError {
        NotFound => "account not found",
        InvalidProof => "invalid proof",
        InvalidAction => "invalid action",
    }

    enum SendMailError {
        NotFound => "account not found",
        SentDirectly => "sent directly",
        SendingToSelf => "sending to self is not allowed",
        MailboxFull => "mailbox full (limit: {MAIL_BOX_CAP})",
    }
}
