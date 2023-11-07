use std::convert::identity;
use std::mem;

use leptos::html::Input;
use leptos::*;
use leptos_router::Redirect;
use protocols::chat::ChatName;

#[leptos::component]
pub fn Chat(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState {
        rchats,
        rkeys,
        rusername,
        wcommands,
        ..
    } = state;

    let Some(_keys) = rkeys.get_untracked() else {
        return view! { <Redirect path="/login"/> }.into_view();
    };

    let side_chat = move |chat: ChatName| {
        view! {
            <div class="sb hov tac bp toe">
                {chat.to_string()}
            </div>
        }
    };

    let message_input = create_node_ref::<Input>();
    let on_message = move |_| {
        let message = message_input.get().unwrap().value();
        let name = rusername.get_untracked();

        log::info!("message");
    };

    view! {
        <crate::Nav rusername/>
        <main class="tbm flx fg1 jcsb">
            <div class="sidebar bhc fg0 rbm oys pr">
                <div class="pa">
                    <div class="bp lsp sc sb tac">
                        rooms
                    </div>
                    <For each=rchats key=mem::copy children=side_chat />

                    <div class="bp toe lsp sc sb tac">
                        dms
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm log as fuck
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm log as fuck
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm log as fuck aksjd laksj dla ksjdlkajs hdlkajs lhk jdsahlksj hdl kjsahdl kjs
                    </div>
                    <div class="sb hov tac bp toe">
                        somedmlogasfucklakjsdlkajhdlkaj
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm log as fuck
                    </div>
                    <div class="sb hov tac bp toe">
                        some dm log as fuck
                    </div>
                </div>
            </div>
            <div class="sc fg1 flx pb fdc">
                <div class="fg1">

                </div>
                <div class="fg0 flx bm bp pc">
                    <input class="fg1 rsb sc hov" type="text" placeholder="mesg..." node_ref=message_input />
                    <svg class="sc lsb hov" on:click=on_message xmlns="http://www.w3.org/2000/svg" height="31" viewBox="0 -960 960 960" width="30">
                        <path d="M120-160v-640l760 320-760 320Zm80-120 474-200-474-200v140l240 60-240 60v140Zm0 0v-400 400Z" />
                    </svg>
                </div>
            </div>
        </main>
    }.into_view()
}
