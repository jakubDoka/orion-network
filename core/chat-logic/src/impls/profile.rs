use {
    super::Nonce,
    crypto::{enc, sign, Serialized},
    libp2p::PeerId,
};

pub const MAIL_BOX_CAP: usize = 1024 * 1024;

component_utils::protocol! {'a:
    #[derive(Clone)]
    struct Profile {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
        last_sig: Serialized<sign::Signature>,
        action: Nonce,
        vault: Vec<u8>,
        mail: Vec<u8>,
        online: Option<PeerId>,
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

impl Profile {
    pub fn read_mail(&mut self) -> &[u8] {
        let slice = unsafe { std::mem::transmute(self.mail.as_slice()) };
        unsafe { self.mail.set_len(0) };
        slice
    }
}

component_utils::gen_simple_error! {
    error FetchProfileError {
        NotFound => "account not found",
    }
}

component_utils::protocol! {'a:
    struct FetchProfileResp {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
    }
}

component_utils::gen_simple_error! {
    error CreateAccountError {
        InvalidProof => "invalid proof",
        AlreadyExists => "account already exists",
    }
}

component_utils::gen_simple_error! {
    error SetVaultError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }
}

component_utils::gen_simple_error! {
    error FetchVaultError {
        NotFound => "account not found",
    }
}

component_utils::gen_simple_error! {
    error ReadMailError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }
}

component_utils::gen_simple_error! {
    error SendMailError {
        NotFound => "account not found",
        MailboxFull => "mailbox full (limit: {MAIL_BOX_CAP})",
    }
}
