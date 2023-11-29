#![feature(iter_collect_into)]
#![allow(non_snake_case)]
#![feature(mem_copy_fn)]
#![feature(macro_metavar_expr)]

use {
    self::web_sys::wasm_bindgen::JsValue,
    crate::{
        chat::{Chat, ChatInvite, Mail},
        login::{Login, Register},
        node::{ChatMeta, Node},
        profile::Profile,
    },
    anyhow::Context,
    chat_logic::{
        ChatName, Identity, Nonce, Proof, ReadMail, RequestDispatch, SendMail, Server, SetVault,
        SubsOwner,
    },
    component_utils::{Codec, Reminder},
    crypto::{
        enc::{self, ChoosenCiphertext},
        sign, TransmutationCircle,
    },
    leptos::{html::Input, leptos_dom::helpers::TimeoutHandle, signal_prelude::*, *},
    leptos_router::*,
    libp2p::futures::FutureExt,
    node::Vault,
    primitives::{contracts::UserIdentity, RawUserName, UserName},
    std::{
        cmp::Ordering,
        fmt::Display,
        future::Future,
        rc::Rc,
        task::{Poll, Waker},
        time::Duration,
    },
    web_sys::js_sys::wasm_bindgen,
};

mod chain;
mod chat;
mod login;
mod node;
mod profile;

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
    sign: sign::KeyPair,
    enc: enc::KeyPair,
    vault: crypto::SharedSecret,
}

crypto::impl_transmute!(RawUserKeys,);

#[derive(Clone)]
struct UserKeys {
    name: UserName,
    sign: sign::KeyPair,
    enc: enc::KeyPair,
    vault: crypto::SharedSecret,
}

impl UserKeys {
    pub fn new(name: UserName) -> Self {
        let sign = sign::KeyPair::new();
        let enc = enc::KeyPair::new();
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
    requests: RwSignal<Option<chat_logic::RequestDispatch<chat_logic::Server>>>,
    vault: RwSignal<Vault>,
    account_nonce: RwSignal<Nonce>,
}

impl State {
    pub fn next_chat_proof(self, chat_name: ChatName) -> Option<chat_logic::Proof> {
        self.keys
            .try_with_untracked(|keys| {
                let keys = keys.as_ref()?;
                self.vault.try_update(|vault| {
                    let chat = vault.chats.get_mut(&chat_name)?;
                    Some(Proof::for_chat(&keys.sign, &mut chat.action_no, chat_name))
                })
            })
            .flatten()
            .flatten()
    }

    pub fn next_profile_proof(self) -> Option<chat_logic::Proof> {
        self.keys
            .try_with_untracked(|keys| {
                let keys = keys.as_ref()?;
                self.account_nonce
                    .try_update(|nonce| Some(Proof::for_profile(&keys.sign, nonce)))
            })
            .flatten()
            .flatten()
    }

    pub fn chat_secret(self, chat_name: ChatName) -> Option<crypto::SharedSecret> {
        self.vault
            .with_untracked(|vault| vault.chats.get(&chat_name).map(|c| c.secret))
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
    let save_vault = move |keys: UserKeys, mut ed: RequestDispatch<Server>| {
        let identity = keys.identity_hash();
        let mut vault_bytes = serialized_vault.get_untracked();
        let proof = state.next_profile_proof().expect("logged in");
        crypto::encrypt(&mut vault_bytes, keys.vault);
        handled_spawn_local(async move {
            ed.dispatch::<SetVault>(identity, (proof, Reminder(&vault_bytes)))
                .await??;
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
        let Some(ed) = state.requests.get_untracked() else {
            return;
        };

        if let Some(handle) = timeout.get_value() {
            handle.clear()
        }
        let handle =
            set_timeout_with_handle(move || save_vault(keys, ed), Duration::from_secs(3)).unwrap();
        timeout.set_value(Some(handle));
    });

    let account_sub = store_value(None::<SubsOwner<SendMail>>);
    create_effect(move |_| {
        let Some(keys) = state.keys.get() else {
            return;
        };

        let enc = keys.enc.clone();
        let handle_chat_invite = move |mail: &[u8]| {
            let Some(Mail::ChatInvite(ChatInvite { chat, cp })) = <_>::decode(&mut &*mail) else {
                anyhow::bail!("failed to decode chat message");
            };

            let secret = enc
                .decapsulate_choosen(ChoosenCiphertext::from_bytes(cp))
                .context("failed to decapsulate invite")?;

            state
                .vault
                .update(|v| _ = v.chats.insert(chat, ChatMeta::from_secret(secret)));

            Ok(())
        };

        handled_spawn_local(async move {
            navigate_to("/");
            let identity = keys.identity_hash();
            let (node, vault, mut dispatch, nonce) = Node::new(keys, wboot_phase)
                .await
                .inspect_err(|_| navigate_to("/login"))
                .context("failed to create node")?;

            let mut dispatch_clone = dispatch.clone();
            let handle_chat_invite_clone = handle_chat_invite.clone();
            handled_spawn_local(async move {
                let proof = state.next_profile_proof().unwrap();
                let list = dispatch_clone
                    .dispatch::<ReadMail>(identity, proof)
                    .await??;

                for mail in chat_logic::unpack_messages_ref(list.0) {
                    handle_chat_invite_clone(mail)?;
                }

                anyhow::Result::Ok(())
            });

            let (mut account, id) = dispatch.subscribe::<SendMail>(identity).unwrap();
            account_sub.set_value(Some(id));
            let listen = async move {
                while let Some(Reminder(mail)) = account.next().await {
                    handle_chat_invite(mail)?;
                }

                anyhow::Result::Ok(())
            };

            state.requests.set_untracked(Some(dispatch));
            state.vault.set_untracked(vault);
            state.account_nonce.set_untracked(nonce);
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
        set_timeout(move || celem.remove(), Duration::from_secs(3000));

        elem
    };

    create_effect(move |_| {
        errors.with(|e| {
            if let Some(e) = e {
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
    #[error("loading profile...")]
    ProfileLoad,
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
        .set_custom_validity(&format!("{message}"));
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

fn _handled_closure(
    f: impl Fn() -> anyhow::Result<()> + 'static + Copy,
) -> impl Fn() + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move || {
        if let Err(e) = f() {
            errors.set(Some(e));
        }
    }
}

fn handled_callback<T>(
    f: impl Fn(T) -> anyhow::Result<()> + 'static + Copy,
) -> impl Fn(T) + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move |v| {
        if let Err(e) = f(v) {
            errors.set(Some(e));
        }
    }
}

fn handled_async_closure<F: Future<Output = anyhow::Result<()>> + 'static>(
    f: impl Fn() -> F + 'static + Copy,
) -> impl Fn() + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move || {
        let fut = f();
        spawn_local(async move {
            if let Err(e) = fut.await {
                errors.set(Some(e));
            }
        });
    }
}

fn handled_async_callback<T, F: Future<Output = anyhow::Result<()>> + 'static>(
    f: impl Fn(T) -> F + 'static + Copy,
) -> impl Fn(T) + 'static + Copy {
    let Errors(errors) = use_context().unwrap();
    move |v| {
        let fut = f(v);
        spawn_local(async move {
            if let Err(e) = fut.await {
                errors.set(Some(e));
            }
        });
    }
}

fn handled_spawn_local(f: impl Future<Output = anyhow::Result<()>> + 'static) {
    let Errors(errors) = use_context().unwrap();
    spawn_local(async move {
        if let Err(e) = f.await {
            errors.set(Some(e));
        }
    });
}
