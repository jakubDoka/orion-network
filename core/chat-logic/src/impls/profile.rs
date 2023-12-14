use {
    crate::{Identity, Topic},
    component_utils::Reminder,
    crypto::{enc, sign, Serialized},
};

pub const MAIL_BOX_CAP: usize = 1024 * 1024;

impl Topic for Identity {
    type Event<'a> = MailEvent<'a>;
}

type MailEvent<'a> = Reminder<'a>;

component_utils::protocol! {'a:
    struct FetchProfileResp {
        sign: Serialized<sign::PublicKey>,
        enc: Serialized<enc::PublicKey>,
    }
}

component_utils::gen_simple_error! {
    error FetchProfileError {
        NotFound => "account not found",
    }

    error CreateAccountError {
        InvalidProof => "invalid proof",
        AlreadyExists => "account already exists",
    }

    error SetVaultError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }

    error FetchVaultError {
        NotFound => "account not found",
    }

    error ReadMailError {
        InvalidProof => "invalid proof",
        NotFound => "account not found",
        InvalidAction => "invalid action",
    }

    error SendMailError {
        NotFound => "account not found",
        MailboxFull => "mailbox full (limit: {MAIL_BOX_CAP})",
    }
}
