#![feature(iter_collect_into)]
#![allow(non_snake_case)]
#![feature(mem_copy_fn)]

use self::web_sys::wasm_bindgen::JsValue;
use crate::chat::Chat;
use crate::login::{Login, Register};
use crate::node::Node;
use crate::profile::Profile;
use leptos::html::Input;
use leptos::signal_prelude::*;
use leptos::*;
use leptos_router::*;
use protocols::chat::{ChatName, UserKeys, UserName};
use std::cmp::Ordering;
use std::fmt::Display;
use std::future::Future;

mod chat;
mod login;
mod node;
mod profile;

pub fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(log::Level::Debug);
    mount_to_body(App)
}

const CHAIN_BOOTSTRAP_NODE: &str = "http://localhost:8700";

#[derive(Clone, Copy)]
struct LoggedState {
    revents: ReadSignal<node::Event>,
    wcommands: WriteSignal<node::Command>,
    chats: RwSignal<Vec<ChatName>>,
    rkeys: ReadSignal<Option<UserKeys>>,
    rusername: ReadSignal<UserName>,
}

fn App() -> impl IntoView {
    let (revents, wevents) = create_signal(node::Event::None);
    let (rcommands, wcommands) = create_signal(node::Command::None);
    let (rkeys, wkeys) = create_signal(None::<UserKeys>);
    let (rusername, wusername) = create_signal(UserName::from("username").unwrap());
    let chats = create_rw_signal(Vec::new());
    let (rboot_phase, wboot_phase) = create_signal(None::<BootPhase>);

    create_effect(move |_| {
        let Some(keys) = rkeys() else {
            return;
        };

        spawn_local(async move {
            navigate_to("/");
            let n = match Node::new(keys, CHAIN_BOOTSTRAP_NODE, wevents, rcommands, wboot_phase)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    log::error!("failed to create node: {e}");
                    return;
                }
            };

            wusername(n.username());
            chats.set(n.chats().collect());

            navigate_to("/chat");
            n.run().await;
        });
    });

    let state = LoggedState {
        revents,
        wcommands,
        chats,
        rkeys,
        rusername,
    };

    let chat = move || view! { <Chat state/> };
    let profile = move || view! { <Profile state/> };
    let login = move || view! { <Login wkeys/> };
    let register = move || view! { <Register wkeys/> };
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
        None => "confused".to_string(),
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
fn Nav(rusername: ReadSignal<UserName>) -> impl IntoView {
    view! {
        <nav class="sc flx jcsb fg0">
            <div class="flx">
                <A class="bf hov bp rsb" href="/chat">/rooms</A>
                <A class="bf hov bp sb" href="/profile">/profile</A>
                <A class="bf hov bp sb" href="/login">/logout</A>
            </div>
            <div class="bp bf hc lsb">
                {move || rusername().to_string()}
            </div>
        </nav>
    }
}

fn load_file(input: HtmlElement<Input>) -> Option<impl Future<Output = Result<Vec<u8>, JsValue>>> {
    use self::web_sys::wasm_bindgen::prelude::Closure;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::task::{Poll, Waker};
    use wasm_bindgen::JsCast;
    use web_sys::*;

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
