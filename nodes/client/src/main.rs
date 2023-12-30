#![feature(iter_collect_into)]
#![allow(non_snake_case)]
#![feature(mem_copy_fn)]
#![feature(macro_metavar_expr)]

use {
    self::{protocol::RequestDispatch, web_sys::wasm_bindgen::JsValue},
    crate::{
        chat::Chat,
        login::{Login, Register},
        node::{
            ChatInvite, ChatMeta, HardenedChatInvite, HardenedChatInvitePayload,
            HardenedJoinRequest, JoinRequestPayload, Mail, MemberMeta, Node,
        },
        profile::Profile,
        protocol::SubsOwner,
    },
    anyhow::Context,
    chat_logic::{ChatName, FetchProfile, Identity, Nonce, Proof, ReadMail, SetVault},
    component_utils::{Codec, Reminder},
    crypto::{
        enc::{self, ChoosenCiphertext, Ciphertext},
        sign, FixedAesPayload, TransmutationCircle,
    },
    leptos::{html::Input, leptos_dom::helpers::TimeoutHandle, signal_prelude::*, *},
    leptos_router::*,
    libp2p::futures::{future::join_all, FutureExt},
    node::Vault,
    primitives::{
        contracts::{StoredUserIdentity, UserIdentity},
        RawUserName, UserName,
    },
    std::{
        cmp::Ordering,
        fmt::Display,
        future::Future,
        rc::Rc,
        task::{Poll, Waker},
        time::Duration,
    },
};

mod chain;
mod chat;
mod db;
mod login;
mod node;
mod profile;
mod protocol;

pub fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(if cfg!(debug_assertions) {
        log::Level::Debug
    } else {
        log::Level::Error
    });
    mount_to_body(App)
}

#[derive(Clone)]
struct RawUserKeys {
    name: RawUserName,
    sign: sign::Keypair,
    enc: enc::Keypair,
    vault: crypto::SharedSecret,
}

crypto::impl_transmute!(RawUserKeys,);

#[derive(Clone)]
struct UserKeys {
    name: UserName,
    sign: sign::Keypair,
    enc: enc::Keypair,
    vault: crypto::SharedSecret,
}

impl UserKeys {
    pub fn new(name: UserName) -> Self {
        let sign = sign::Keypair::new();
        let enc = enc::Keypair::new();
        let vault = crypto::new_secret();
        Self {
            name,
            sign,
            enc,
            vault,
        }
    }

    pub fn identity_hash(&self) -> Identity {
        crypto::hash::new(&self.sign.public_key())
    }

    pub fn to_identity(&self) -> UserIdentity {
        UserIdentity {
            sign: self.sign.public_key(),
            enc: self.enc.public_key(),
        }
    }

    pub fn try_from_raw(raw: RawUserKeys) -> Option<Self> {
        let RawUserKeys {
            name,
            sign,
            enc,
            vault,
        } = raw;
        Some(Self {
            name: component_utils::array_to_arrstr(name)?,
            sign,
            enc,
            vault,
        })
    }

    pub fn into_raw(self) -> RawUserKeys {
        let Self {
            name,
            sign,
            enc,
            vault,
        } = self;
        RawUserKeys {
            name: component_utils::arrstr_to_array(name),
            sign,
            enc,
            vault,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct State {
    keys: RwSignal<Option<UserKeys>>,
    requests: StoredValue<Option<RequestDispatch>>,
    vault: RwSignal<Vault>,
    vault_version: StoredValue<Nonce>,
    mail_action: StoredValue<Nonce>,
    hardened_messages: RwSignal<Option<(ChatName, db::Message)>>,
}

impl State {
    pub fn next_chat_proof(
        self,
        chat_name: ChatName,
        nonce: Option<Nonce>,
    ) -> Option<chat_logic::Proof> {
        self.keys
            .try_with_untracked(|keys| {
                let keys = keys.as_ref()?;
                self.vault.try_update(|vault| {
                    let chat = vault.chats.get_mut(&chat_name)?;
                    chat.action_no.0 = nonce.map_or(chat.action_no.0, |n| n + 1);
                    Some(Proof::for_chat(&keys.sign, &mut chat.action_no, chat_name))
                })
            })
            .flatten()
            .flatten()
    }

    pub fn next_profile_proof(self, vault: &[u8]) -> Option<chat_logic::Proof> {
        self.keys
            .try_with_untracked(|keys| {
                let keys = keys.as_ref()?;
                self.vault_version
                    .try_update_value(|nonce| Some(Proof::for_vault(&keys.sign, nonce, vault)))
            })
            .flatten()
            .flatten()
    }

    pub fn chat_secret(self, chat_name: ChatName) -> Option<crypto::SharedSecret> {
        self.vault
            .with_untracked(|vault| vault.chats.get(&chat_name).map(|c| c.secret))
    }

    fn next_mail_proof(&self) -> Option<chat_logic::Proof> {
        self.keys
            .try_with_untracked(|keys| {
                let keys = keys.as_ref()?;
                self.mail_action
                    .try_update_value(|nonce| Some(Proof::for_mail(&keys.sign, nonce)))
            })
            .flatten()
            .flatten()
    }
}

fn App() -> impl IntoView {
    let (rboot_phase, wboot_phase) = create_signal(None::<BootPhase>);
    let (errors, set_errors) = create_signal(None::<anyhow::Error>);
    provide_context(Errors(set_errors));
    let state = State::default();

    let serialized_vault = create_memo(move |_| {
        log::debug!("vault serialized");
        state.vault.with(Codec::to_bytes)
    });
    let timeout = store_value(None::<TimeoutHandle>);
    let save_vault = move |keys: UserKeys, mut ed: RequestDispatch| {
        let mut vault_bytes = serialized_vault.get_untracked();
        crypto::encrypt(&mut vault_bytes, keys.vault);
        let proof = state.next_profile_proof(&vault_bytes).expect("logged in");
        handled_spawn_local("saving vault", async move {
            ed.dispatch::<SetVault>((proof, Reminder(&vault_bytes)))
                .await
                .context("setting vault")?;
            log::debug!("saved vault");
            Ok(())
        });
    };
    create_effect(move |init| {
        serialized_vault.track();
        if init.is_none() {
            return;
        }
        let Some(keys) = state.keys.get_untracked() else {
            return;
        };
        let Some(ed) = state.requests.get_value() else {
            return;
        };

        if let Some(handle) = timeout.get_value() {
            handle.clear()
        }
        let handle =
            set_timeout_with_handle(move || save_vault(keys, ed), Duration::from_secs(3)).unwrap();
        timeout.set_value(Some(handle));
    });

    async fn notify_about_invite(
        name: UserName,
        chat: ChatName,
        my_name: UserName,
        enc: enc::Keypair,
        identity: Identity,
        mut dispatch: RequestDispatch,
    ) -> anyhow::Result<(UserName, MemberMeta)> {
        let client = chain::node(my_name).await?;
        let identity_hashes = client
            .get_profile_by_name(chain::user_contract(), name)
            .await?
            .map(StoredUserIdentity::from_bytes)
            .context("user not found")?;
        let pf = dispatch
            .dispatch::<FetchProfile>(identity_hashes.sign)
            .await
            .context("fetching profile")?;

        let (cp, secret) = enc.encapsulate(&enc::PublicKey::from_bytes(pf.enc));

        let payload = JoinRequestPayload {
            chat: component_utils::arrstr_to_array(chat),
            name: component_utils::arrstr_to_array(name),
            identity,
        }
        .into_bytes();
        let invite = Mail::HardenedJoinRequest(HardenedJoinRequest {
            cp: cp.into_bytes(),
            payload: unsafe {
                std::mem::transmute(FixedAesPayload::new(payload, secret, crypto::ASOC_DATA))
            },
        })
        .to_bytes();

        dispatch
            .dispatch_mail((identity_hashes.sign, Reminder(&invite)))
            .await
            .context("sending invite")?;

        Ok((name, MemberMeta {
            secret,
            identity: identity_hashes.sign,
        }))
    }

    let handle_mail = move |mut raw_mail: &[u8],
                            dispatch: &RequestDispatch,
                            enc: enc::Keypair,
                            my_id: Identity,
                            my_name: UserName,
                            new_messages: &mut Vec<db::Message>| {
        let Some(mail) = Mail::decode(&mut raw_mail) else {
            anyhow::bail!("failed to decode chat message");
        };
        assert!(raw_mail.is_empty());

        match mail {
            Mail::ChatInvite(ChatInvite { chat, cp }) => {
                let secret = enc
                    .decapsulate_choosen(ChoosenCiphertext::from_bytes(cp))
                    .context("failed to decapsulate invite")?;

                state
                    .vault
                    .update(|v| _ = v.chats.insert(chat, ChatMeta::from_secret(secret)));
            }
            Mail::HardenedJoinRequest(HardenedJoinRequest { cp, payload }) => {
                log::debug!("handling hardened join request");
                let secret = enc
                    .decapsulate(Ciphertext::from_bytes(cp))
                    .context("failed to decapsulate invite")?;

                let payload: FixedAesPayload<{ std::mem::size_of::<JoinRequestPayload>() }> =
                    unsafe { std::mem::transmute(payload) };

                let JoinRequestPayload {
                    chat,
                    name,
                    identity,
                } = payload
                    .decrypt(secret, crypto::ASOC_DATA)
                    .map(JoinRequestPayload::from_bytes)
                    .context("failed to decrypt join request payload")?;

                let chat = component_utils::array_to_arrstr(chat).context("invalid chat name")?;
                let name = component_utils::array_to_arrstr(name).context("invalid name")?;

                if state.vault.with_untracked(|v| !v.chats.contains_key(&chat)) {
                    anyhow::bail!("request to join chat we dont have");
                }

                state.vault.update(|v| {
                    log::debug!("adding member to hardened chat");
                    let chat = v.hardened_chats.get_mut(&chat).expect("we just checked");
                    chat.members.insert(name, MemberMeta { secret, identity });
                })
            }
            Mail::HardenedChatInvite(HardenedChatInvite { cp, payload }) => {
                log::debug!("handling hardened chat invite");
                let enc = enc.clone();
                let secret = enc
                    .decapsulate(Ciphertext::from_bytes(cp))
                    .context("failed to decapsulate hardened invite")?;

                let mut payload = payload.0.to_owned();
                let payload = crypto::decrypt(&mut payload, secret)
                    .context("failed to decrypt hardened invite")?;
                let HardenedChatInvitePayload {
                    chat,
                    inviter,
                    inviter_id,
                    members,
                } = <_>::decode(&mut &*payload).context("failed to decode hardened invite")?;
                let dispatch = dispatch.clone();
                handled_spawn_local("inviting hardened user user", async move {
                    let members = join_all(members.into_iter().map(|id| {
                        notify_about_invite(id, chat, my_name, enc.clone(), my_id, dispatch.clone())
                    }))
                    .await
                    .into_iter()
                    .collect::<anyhow::Result<Vec<_>>>()?;

                    state.vault.update(|v| {
                        let meta = v.hardened_chats.entry(chat);
                        members
                            .into_iter()
                            .for_each(|(name, secret)| _ = meta.members.insert(name, secret));
                        meta.members.insert(inviter, MemberMeta {
                            secret,
                            identity: inviter_id,
                        });
                    });

                    Ok(())
                });
            }
            Mail::HardenedChatMessage(msg) => {
                let mut message = msg.content.0.to_owned();
                state.vault.update(|v| {
                    let Some((&chat, meta)) = v.hardened_chats.iter_mut().find(|(c, _)| {
                        crypto::hash::new_with_nonce(c.as_bytes(), msg.nonce) == msg.chat
                    }) else {
                        log::warn!("received message for unknown chat");
                        return;
                    };

                    let Some((&sender, content)) =
                        meta.members.iter_mut().find_map(|(name, mm)| {
                            let decrypted = crypto::decrypt(&mut message, mm.secret)?;
                            Some((name, std::str::from_utf8(decrypted).ok().unwrap().into()))
                        })
                    else {
                        log::warn!("received message for chat we are not in");
                        return;
                    };

                    let message = db::Message {
                        sender,
                        owner: my_name,
                        content,
                        chat,
                    };
                    new_messages.push(message.clone());
                    state.hardened_messages.set(Some((chat, message)));
                });
            }
        }

        Ok(())
    };

    let account_sub = store_value(None::<SubsOwner<Identity>>);
    create_effect(move |_| {
        let Some(keys) = state.keys.get() else {
            return;
        };

        let enc = keys.enc.clone();
        let my_name = keys.name;
        let my_id = keys.identity_hash();

        handled_spawn_local("initializing node", async move {
            navigate_to("/");
            let identity = keys.identity_hash();
            let (node, vault, mut dispatch, vault_version, mail_action) =
                Node::new(keys, wboot_phase)
                    .await
                    .inspect_err(|_| navigate_to("/login"))?;

            let mut dispatch_clone = dispatch.clone();
            let cloned_enc = enc.clone();
            handled_spawn_local("reading mail", async move {
                let proof = state.next_mail_proof().unwrap();
                let inner_dispatch = dispatch_clone.clone();
                let list = dispatch_clone.dispatch::<ReadMail>(proof).await?;

                let mut new_messages = Vec::new();
                for mail in chat_logic::unpack_messages_ref(list.0) {
                    handle_error(
                        handle_mail(
                            mail,
                            &inner_dispatch,
                            cloned_enc.clone(),
                            my_id,
                            my_name,
                            &mut new_messages,
                        )
                        .context("receiving a mail"),
                    );
                }
                db::save_messages(new_messages).await
            });

            let (mut account, id) = dispatch.subscribe(identity).unwrap();
            let dispatch_clone = dispatch.clone();
            account_sub.set_value(Some(id));
            let listen = async move {
                while let Some(Reminder(mail)) = account.next().await {
                    let mut new_messages = Vec::new();
                    handle_error(
                        handle_mail(
                            mail,
                            &dispatch_clone,
                            enc.clone(),
                            my_id,
                            my_name,
                            &mut new_messages,
                        )
                        .context("receiving a mail"),
                    );
                    handle_error(db::save_messages(new_messages).await);
                }

                anyhow::Result::Ok(())
            };

            state.requests.set_value(Some(dispatch));
            state.vault.set_untracked(vault);
            state.vault_version.set_value(vault_version);
            state.mail_action.set_value(mail_action);
            navigate_to("/chat");

            libp2p::futures::select! {
                e = node.run().fuse() => e,
                e = listen.fuse() => e,
            }
        });
    });

    let chat = move || view! { <Chat state/> };
    let profile = move || view! { <Profile state/> };
    let login = move || view! { <Login state/> };
    let register = move || view! { <Register state/> };
    let boot = move || view! { <Boot rboot_phase/> };

    view! {
        <Router>
        <Routes>
            <Route path="/chat/:id?" view=chat></Route>
            <Route path="/profile" view=profile></Route>
            <Route path="/login" view=login></Route>
            <Route path="/register" view=register></Route>
            <Route path="/" view=boot></Route>
        </Routes>
        </Router>
        <ErrorPanel errors/>
    }
}

#[component]
fn ErrorPanel(errors: ReadSignal<Option<anyhow::Error>>) -> impl IntoView {
    let error_nodes = create_node_ref::<html::Div>();
    let error_message = move |message: String| {
        let elem = view! {
            <div class="tbm" onclick="this.remove()">
                <div class="ec hov bp pea" style="cursor: pointer">{message}</div>
            </div>
        };

        let celem = elem.clone();
        set_timeout(move || celem.remove(), Duration::from_secs(7));

        elem
    };

    create_effect(move |_| {
        errors.with(|e| {
            if let Some(e) = e {
                log::error!("{e:#}");
                error_nodes
                    .get_untracked()
                    .unwrap()
                    .append_child(&error_message(format!("{e:#}")))
                    .unwrap();
            }
        });
    });

    view! {
        <div class="fsc jcfe flx pen">
            <div class="bm" style="align-self: flex-end;" node_ref=error_nodes />
        </div>
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[repr(u8)]
pub enum BootPhase {
    #[error("fetching nodes and profile from chain...")]
    FetchNodesAndProfile,
    #[error("initiating orion connection...")]
    InitiateConnection,
    #[error("bootstrapping kademlia...")]
    Bootstrapping,
    #[error("collecting server keys... ({0} left)")]
    CollecringKeys(usize),
    #[error("initiating search path...")]
    InitialRoute,
    #[error("searching profile...")]
    ProfileSearch,
    #[error("opening route to profile...")]
    ProfileOpen,
    #[error("loading vault...")]
    VaultLoad,
    #[error("creating new profile...")]
    ProfileCreate,
    #[error("searching chats...")]
    ChatSearch,
    #[error("loading chats...")]
    ChatLoad,
    #[error("ready")]
    ChatRun,
}

impl BootPhase {
    fn discriminant(&self) -> u8 {
        // SAFETY: Because `Self` is marked `repr(u8)`, its layout is a `repr(C)` `union`
        // between `repr(C)` structs, each of which has the `u8` discriminant as its first
        // field, so we can read the discriminant without offsetting the pointer.
        unsafe { *<*const _>::from(self).cast::<u8>() }
    }
}

#[component]
fn Boot(rboot_phase: ReadSignal<Option<BootPhase>>) -> impl IntoView {
    let phases = (0..BootPhase::ChatRun.discriminant())
        .map(|i| {
            let margin = if i == 0 { "" } else { "lbm" };
            let compute_class = move || {
                rboot_phase.with(|phase| match phase.map(|p| i.cmp(&(p.discriminant()))) {
                    Some(Ordering::Less) => "bar-loaded",
                    Some(Ordering::Equal) => "bar-loading",
                    Some(Ordering::Greater) => "bar-unloaded",
                    None => "",
                })
            };
            view! { <span class=move || format!("bp hc fg1 tac {} {margin}", compute_class()) /> }
        })
        .collect_view();

    let message = move || match rboot_phase() {
        Some(s) => format!("{s}"),
        None => {
            navigate_to("/login");
            "confused".to_string()
        }
    };

    view! {
        <main class="ma sc bp">
            <h1>Initiating connections</h1>
            <p>{message}</p>
            <div class="flx bp pc">
                {phases}
            </div>
        </main>
    }
}

#[component]
fn Nav(my_name: UserName) -> impl IntoView {
    let menu = create_node_ref::<html::Div>();
    let on_menu_toggle = move |_| {
        let menu = menu.get_untracked().unwrap();
        menu.set_hidden(!menu.hidden());
    };

    let uname = move || my_name.to_string();

    view! {
        <nav class="sc flx fdc fg0 phone-only">
            <div class="flx jcsb">
                <button class="rsb hov sc nav-menu-button" on:click=on_menu_toggle>/menu</button>
                <div class="bp bf hc lsb">{uname}</div>
            </div>
            <div class="flx fdc tsm" hidden node_ref=menu>
                <A class="bf hov bp bsb" href="/chat">/rooms</A>
                <A class="bf hov bp sb" href="/profile">/profile</A>
                <A class="bf hov bp tsb" href="/login">/logout</A>
            </div>
        </nav>

        <nav class="sc flx jcsb fg0 desktop-only">
            <div class="flx">
                <A class="bf hov bp rsb" href="/chat">/rooms</A>
                <A class="bf hov bp sb" href="/profile">/profile</A>
                <A class="bf hov bp sb" href="/login">/logout</A>
            </div>
            <div class="bp bf hc lsb">{uname}</div>
        </nav>
    }
}

fn load_file(input: HtmlElement<Input>) -> Option<impl Future<Output = Result<Vec<u8>, JsValue>>> {
    use {
        self::web_sys::wasm_bindgen::prelude::Closure, std::cell::RefCell, wasm_bindgen::JsCast,
        web_sys::*,
    };

    struct FileFutureInner {
        loaded: Option<Result<Vec<u8>, JsValue>>,
        waker: Option<Waker>,
        closure: Option<Closure<dyn FnMut(Event)>>,
    }

    let file_list = input.files()?;
    let file = file_list.get(0)?;

    let filereader = FileReader::new().unwrap().dyn_into::<FileReader>().ok()?;
    filereader.read_as_array_buffer(&file).ok()?;

    let inner = Rc::new(RefCell::new(FileFutureInner {
        loaded: None,
        waker: None,
        closure: None,
    }));

    let captured_inner = inner.clone();
    let captured_reader = filereader.clone();
    let closure = Closure::wrap(Box::new(move |_: Event| {
        let mut inner = captured_inner.borrow_mut();
        let data = match captured_reader.result() {
            Ok(data) => data,
            Err(error) => {
                inner.loaded = Some(Err(error));
                if let Some(waker) = inner.waker.take() {
                    waker.wake();
                }
                return;
            }
        };

        let data = data.dyn_into::<js_sys::ArrayBuffer>().unwrap();
        let js_data = js_sys::Uint8Array::new(&data);
        let data = js_data.to_vec();

        inner.loaded = Some(Ok(data));
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }) as Box<dyn FnMut(_)>);
    filereader.set_onloadend(Some(closure.as_ref().unchecked_ref()));
    inner.borrow_mut().closure = Some(closure);

    let inner = Rc::downgrade(&inner);
    Some(std::future::poll_fn(move |cx| {
        let Some(inner) = inner.upgrade() else {
            return Poll::Ready(Err("fuck".into()));
        };
        let mut inner = inner.borrow_mut();
        if let Some(loaded) = inner.loaded.take() {
            return Poll::Ready(loaded);
        }
        inner.waker = Some(cx.waker().clone());
        Poll::Pending
    }))
}

fn report_validity(elem: NodeRef<Input>, message: impl Display) {
    elem.get_untracked()
        .unwrap()
        .set_custom_validity(&format!("{message:#}"));
    elem.get_untracked().unwrap().report_validity();
}

fn get_value(elem: NodeRef<Input>) -> String {
    elem.get_untracked().unwrap().value()
}

fn navigate_to(path: impl Display) {
    leptos_router::use_navigate()(&format!("{path}"), Default::default());
}

fn not(signal: impl Fn() -> bool + Copy) -> impl Fn() -> bool + Copy {
    move || !signal()
}

fn handle_js_err(jv: JsValue) -> anyhow::Error {
    anyhow::anyhow!("{jv:?}")
}

#[derive(Clone, Copy)]
struct Errors(WriteSignal<Option<anyhow::Error>>);

fn handle_error<T>(r: anyhow::Result<T>) -> Option<T> {
    let Errors(errors) = use_context().unwrap();
    r.map_err(|e| errors.set(Some(e))).ok()
}

fn _handled_closure(
    context: &'static str,
    f: impl Fn() -> anyhow::Result<()> + 'static + Copy,
) -> impl Fn() + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move || {
        if let Err(e) = f().context(context) {
            errors.set(Some(e));
        }
    }
}

fn handled_callback<T>(
    context: &'static str,
    f: impl Fn(T) -> anyhow::Result<()> + 'static + Copy,
) -> impl Fn(T) + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move |v| {
        if let Err(e) = f(v).context(context) {
            errors.set(Some(e));
        }
    }
}

fn handled_async_closure<F: Future<Output = anyhow::Result<()>> + 'static>(
    context: &'static str,
    f: impl Fn() -> F + 'static + Copy,
) -> impl Fn() + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move || {
        let fut = f();
        spawn_local(async move {
            if let Err(e) = fut.await.context(context) {
                errors.set(Some(e));
            }
        });
    }
}

fn handled_async_callback<T, F: Future<Output = anyhow::Result<()>> + 'static>(
    context: &'static str,
    f: impl Fn(T) -> F + 'static + Copy,
) -> impl Fn(T) + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move |v| {
        let fut = f(v);
        spawn_local(async move {
            if let Err(e) = fut.await.context(context) {
                errors.set(Some(e));
            }
        });
    }
}

fn handled_spawn_local(
    context: &'static str,
    f: impl Future<Output = anyhow::Result<()>> + 'static,
) {
    let Errors(errors) = use_context().unwrap();
    spawn_local(async move {
        if let Err(e) = f.await.context(context) {
            errors.set(Some(e));
        }
    });
}
