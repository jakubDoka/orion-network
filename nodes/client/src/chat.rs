use std::mem;

use leptos::html::Input;
use leptos::*;
use leptos_router::Redirect;
use protocols::chat::{ChatName, CreateChatErrorData, UserName};
use web_sys::KeyboardEvent;

use crate::node::MessageContent;
use crate::{get_value, navigate_to, node, report_validity};

#[leptos::component]
pub fn Chat(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState {
        chats,
        rkeys,
        rusername,
        revents,
        wcommands,
        ..
    } = state;

    let Some(_keys) = rkeys.get_untracked() else {
        return view! { <Redirect path="/login"/> }.into_view();
    };

    let selected: Option<ChatName> = leptos_router::use_query_map()
        .with_untracked(|m| m.get("id").and_then(|v| v.as_str().try_into().ok()))
        .filter(|v| chats.with(|chats| chats.contains(v)));
    let current_chat = create_rw_signal(selected);

    let side_chat = move |chat: ChatName| {
        let select_chat = move |_| {
            current_chat.set(Some(chat));
            navigate_to(format_args!("/chat/{chat}"));
        };
        let selected = move || current_chat.get() == Some(chat);
        let not_selected = move || current_chat.get() != Some(chat);
        view! { <div class="sb tac bp toe" class:hc=selected class:hov=not_selected on:click=select_chat> {chat.to_string()} </div> }
    };

    let messages = create_node_ref::<leptos::html::Div>();
    let message_view = move |username: UserName, content: MessageContent| {
        let my_name = rusername.get_untracked() == username;
        view! {
            <div class="hc bp flx tbm" class:pc=my_name>
                <div class="hc" class:pc=my_name>{username.to_string()}:</div>
                <div class="lbp hc" class:pc=my_name>{content.to_string()}</div>
            </div>
        }
    };
    let append_message = move |username: UserName, content: MessageContent| {
        let messages = messages.get_untracked().expect("universe to work");
        let message = message_view(username, content);
        messages.append_child(&message).unwrap();
    };

    let hidden = create_rw_signal(true);
    let bts_disabled = create_rw_signal(false);
    let name_input = create_node_ref::<Input>();

    create_effect(move |_| match revents() {
        node::Event::ChatCreated(chat) => {
            chats.update(|chats| chats.push(chat));
            hidden.set(true);
            bts_disabled.set(false);
        }
        node::Event::CannotCreateChat(CreateChatErrorData { err, name }) => {
            report_validity(name_input, format_args!("failed to create '{name}': {err}"));
            bts_disabled.set(false);
        }
        node::Event::NewMessage {
            chat,
            name,
            content,
        } if current_chat.get_untracked() == Some(chat) => append_message(name, content),
        _ => {}
    });

    let on_open = move |_| hidden.set(false);
    let on_close = move |_| hidden.set(true);
    let on_create = move |_| {
        let Ok(chat) = ChatName::try_from(get_value(name_input).as_str()) else {
            return;
        };
        if chats.with(|chats| chats.contains(&chat)) {
            report_validity(
                name_input,
                format_args!("chat '{chat}' already exists and you have it"),
            );
            return;
        }
        wcommands(node::Command::CreateChat(chat));
        bts_disabled.set(true);
    };

    let message_input = create_node_ref::<Input>();
    let on_input = move |e: web_sys::KeyboardEvent| {
        log::info!(
            "key: {} {} {}",
            e.key_code(),
            '\n' as u32,
            e.get_modifier_state("Shift")
        );
        if e.key_code() != '\r' as u32 || e.get_modifier_state("Shift") {
            return;
        }

        let chat = current_chat.get_untracked().expect("universe to work");
        log::info!("sending message to {chat}");

        let content = get_value(message_input);
        if content.is_empty() {
            return;
        }

        log::info!("sending message: {}", content);

        wcommands(node::Command::SendMessage { chat, content });
    };

    view! {
        <crate::Nav rusername/>
        <main class="tbm flx fg1 jcsb">
            <div class="sidebar bhc fg0 rbm oys pr">
                <div class="pa">
                    <div class="bp lsp sc sb tac">
                        "chats" <button class="hov sf pc lsm" on:click=on_open >+</button>
                    </div>
                    <For each=chats key=mem::copy children=side_chat />
                    <div class="tac bm" hidden=move || !chats.with(Vec::is_empty)>"no chats yet"</div>
                    /*
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
                    */
                </div>
            </div>
            <div class="sc fg1 flx pb fdc" hidden=move || current_chat.with(Option::is_none)>
                <div class="fg1 flx fdc sc pr oys fy"><div class="fg1 flx fdc bp sc pa fy" node_ref=messages>
                </div></div>
                <div class="fg0 flx bm bp pc">
                    <input class="fg1 sc hov" type="text" placeholder="mesg..." node_ref=message_input on:keyup=on_input />
                </div>
            </div>
            <div class="sc fg1 flx pb fdc" hidden=move || current_chat.with(Option::is_some)>
                <div class="ma">"no chat selected"</div>
            </div>
        </main>

        <div class="fsc flx blr sb" hidden=hidden>
            <div class="sc flx fdc bp ma bsha">
                <input class="pc hov bp" type="text" placeholder="chat name..." maxlength=32 required node_ref=name_input />
                <input class="pc hov bp tbm" type="button" value="create" disabled=bts_disabled on:click=on_create  />
                <input class="pc hov bp tbm" type="button" value="cancel" disabled=bts_disabled on:click=on_close />
            </div>
        </div>
    }.into_view()
}
