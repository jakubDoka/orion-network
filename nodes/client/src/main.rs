#![feature(iter_collect_into)]
#![allow(non_snake_case)]
#![feature(mem_copy_fn)]
#![feature(macro_metavar_expr)]

use chat_logic::{ChatName, Proof};

use {
    self::web_sys::wasm_bindgen::JsValue,
    crate::{
        chat::Chat,
        login::{Login, Register},
        node::Node,
        profile::Profile,
    },
    chain_api::{ContractId, TransactionHandler},
    chat_logic::Nonce,
    crypto::{enc, sign},
    leptos::{html::Input, signal_prelude::*, *},
    leptos_router::*,
    libp2p::Multiaddr,
    node::Vault,
    primitives::{contracts::UserIdentity, RawUserName, UserName},
    std::{
        cmp::Ordering,
        fmt::Display,
        future::Future,
        rc::Rc,
        str::FromStr,
        task::{Poll, Waker},
    },
    web_sys::js_sys::wasm_bindgen,
};

mod chat;
mod login;
mod node;
mod profile;

pub fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(App)
}

async fn sign_with_wallet(payload: &str) -> Result<Vec<u8>, JsValue> {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_namespace = integration)]
        async fn sign(data: &str) -> Result<JsValue, JsValue>;
    }

    let sig = sign(payload).await?;
    let sig = sig.as_string().ok_or("user did something very wrong")?;
    let sig = sig.trim_start_matches("0x01");
    hex::decode(sig).map_err(|e| e.to_string().into())
}

async fn get_account_id(name: &str) -> Result<String, JsValue> {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_namespace = integration)]
        async fn address(name: &str) -> Result<JsValue, JsValue>;
    }

    let id = address(name).await?;
    id.as_string().ok_or("user, pleas stop").map_err(Into::into)
}

struct WebSigner(UserName);

impl TransactionHandler for WebSigner {
    async fn account_id_async(&self) -> Result<chain_api::AccountId, chain_api::Error> {
        let id = get_account_id(&self.0)
            .await
            .map_err(|e| chain_api::Error::Other(format!("{e:?}")))?;
        chain_api::AccountId::from_str(&id)
            .map_err(|e| chain_api::Error::Other(format!("invalid id received: {e}")))
    }

    async fn handle(
        &self,
        inner: &chain_api::InnerClient,
        call: impl chain_api::TxPayload,
    ) -> Result<(), chain_api::Error> {
        let account_id = self.account_id_async().await?;
        let nonce = inner.get_nonce(&account_id).await?;
        let genesis_hash = chain_api::encode_then_hex(&inner.client.genesis_hash());
        // These numbers aren't SCALE encoded; their bytes are just converted to hex:
        let spec_version =
            chain_api::to_hex(&inner.client.runtime_version().spec_version.to_be_bytes());
        let transaction_version = chain_api::to_hex(
            &inner
                .client
                .runtime_version()
                .transaction_version
                .to_be_bytes(),
        );
        let nonce_enc = chain_api::to_hex(&nonce.to_be_bytes());
        let mortality_checkpoint = chain_api::encode_then_hex(&inner.client.genesis_hash());
        let era = chain_api::immortal_era();
        let method = chain_api::to_hex(call.encode_call_data(&inner.client.metadata())?);
        let signed_extensions: Vec<String> = inner
            .client
            .metadata()
            .extrinsic()
            .signed_extensions()
            .iter()
            .map(|e| e.identifier().to_string())
            .collect();
        let tip = chain_api::encode_tip(0u128);
        let payload = chain_api::json!({
            "specVersion": spec_version,
            "transactionVersion": transaction_version,
            "address": account_id.to_string(),
            "blockHash": mortality_checkpoint,
            "blockNumber": "0x00000000",
            "era": era,
            "genesisHash": genesis_hash,
            "method": method,
            "nonce": nonce_enc,
            "signedExtensions": signed_extensions,
            "tip": tip,
            "version": 4,
        });

        let signature = sign_with_wallet(&payload.to_string())
            .await
            .map_err(|e| chain_api::Error::Other(format!("{e:?}")))?;

        let signature = signature
            .try_into()
            .map_err(|_| chain_api::Error::Other("signature has invalid size".into()))
            .map(chain_api::new_signature)?;

        let tx = inner.client.tx();

        tx.validate(&call)?;
        let unsigned_payload =
            tx.create_partial_signed_with_nonce(&call, nonce, Default::default())?;

        unsigned_payload
            .sign_with_address_and_signature(&account_id.into(), &signature.into())
            .submit_and_watch()
            .await?
            .wait_for_in_block()
            .await?
            .wait_for_success()
            .await
            .map(drop)
    }
}

macro_rules! build_env {
    ($vis:vis $name:ident) => {
        #[cfg(feature = "building")]
        $vis const $name: &str = env!(stringify!($name));
        #[cfg(not(feature = "building"))]
        $vis const $name: &str = "";
    };
}

async fn chain_node(name: UserName) -> Result<chain_api::Client<WebSigner>, chain_api::Error> {
    build_env!(BOOTSTRAP_NODE);
    chain_api::Client::with_signer(BOOTSTRAP_NODE, WebSigner(name)).await
}

fn boot_node() -> Multiaddr {
    build_env!(BOOTSTRAP_NODE);
    BOOTSTRAP_NODE.parse().unwrap()
}

fn user_contract() -> ContractId {
    build_env!(USER_CONTRACT);
    ContractId::from_str(USER_CONTRACT).unwrap()
}

fn node_contract() -> ContractId {
    build_env!(NODE_CONTRACT);
    ContractId::from_str(NODE_CONTRACT).unwrap()
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

#[derive(Default)]
struct UserState {
    keys: Option<UserKeys>,
    requests: Option<chat_logic::RequestDispatch<chat_logic::Server>>,
    vault: Vault,
    account_nonce: Nonce,
}

impl UserState {
    pub fn next_chat_proof(&mut self, chat_name: ChatName) -> Option<chat_logic::Proof> {
        self.vault
            .next_chat_proof(chat_name, &self.keys.as_ref()?.sign)
    }

    pub fn next_profile_proof(&mut self) -> Option<chat_logic::Proof> {
        Some(Proof::for_profile(
            &self.keys.as_ref()?.sign,
            &mut self.account_nonce,
        ))
    }
}

#[derive(Clone, Copy)]
struct LoggedState {
    user_state: RwSignal<UserState>,
}

fn App() -> impl IntoView {
    let (rboot_phase, wboot_phase) = create_signal(None::<BootPhase>);
    let user_state = create_rw_signal(UserState::default());

    create_effect(move |_| {
        let Some(keys) = user_state.with(|u| u.keys.clone()) else {
            return;
        };

        spawn_local(async move {
            navigate_to("/");
            let (n, us) = match Node::new(keys, wboot_phase).await {
                Ok(n) => n,
                Err(e) => {
                    log::error!("failed to create node: {e}");
                    return;
                }
            };

            user_state.set(us);
            navigate_to("/chat");
            n.run().await;
        });
    });

    let state = LoggedState { user_state };

    let chat = move || view! { <Chat state/> };
    let profile = move || view! { <Profile state/> };
    let login = move || view! { <Login user_state/> };
    let register = move || view! { <Register user_state/> };
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
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[repr(u8)]
pub enum BootPhase {
    #[error("fetch topology...")]
    FetchTopology,
    #[error("initiating orion connection...")]
    InitiateConnection,
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

#[component]
fn Boot(rboot_phase: ReadSignal<Option<BootPhase>>) -> impl IntoView {
    let phases = (0..BootPhase::ChatRun as usize)
        .map(|i| {
            let margin = if i == 0 { "" } else { "lbm" };
            let compute_class = move || {
                rboot_phase.with(|phase| match phase.map(|p| i.cmp(&(p as usize))) {
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
