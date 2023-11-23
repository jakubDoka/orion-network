use leptos::html::Input;
use leptos::*;
use leptos_router::Redirect;
use protocols::chat::{AddMember, ChatName, CreateChatErrorData, UserName};
use protocols::contracts::UserIdentity;

use crate::node;
use crate::node::MessageContent;

fn is_at_bottom(messages_div: HtmlElement<leptos::html::Div>) -> bool {
    let scroll_bottom = messages_div.scroll_top();
    let scroll_height = messages_div.scroll_height();
    let client_height = messages_div.client_height();

    let prediction = 200;
    scroll_height - client_height <= scroll_bottom + prediction
}

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
        .filter(|v| chats.with_untracked(|chats| chats.contains(v)));
    if let Some(selected) = selected {
        wcommands(node::Command::FetchMessages(selected, true));
    }
    let current_chat = create_rw_signal(selected);
    let red_all_messages = create_rw_signal(false);

    let messages = create_node_ref::<leptos::html::Div>();
    let message_view = move |username: UserName, content: MessageContent| {
        let my_message = rusername.get_untracked() == username;
        let justify = if my_message { "right" } else { "left" };
        let color = if my_message { "hc" } else { "pc" };
        view! {
            <div class="tbm flx" style=("justify-content", justify)>
                <div class=format!("bp flx {color}")>
                    <div class=color>{username.to_string()}:</div>
                    <div class=format!("lbp {color}")>{content.to_string()}</div>
                </div>
            </div>
        }
    };
    let append_message = move |username: UserName, content: MessageContent| {
        let messages = messages.get_untracked().expect("universe to work");
        let message = message_view(username, content);
        messages.append_child(&message).unwrap();
    };
    let prepend_message = move |username: UserName, content: MessageContent| {
        let messages = messages.get_untracked().expect("universe to work");
        let message = message_view(username, content);
        messages
            .insert_before(&message, messages.first_child().as_ref())
            .unwrap();
    };

    let side_chat = move |chat: ChatName| {
        let select_chat = move |_| {
            current_chat.set(Some(chat));
            crate::navigate_to(format_args!("/chat/{chat}"));
            let messages = messages.get_untracked().expect("universe to work");
            messages.set_inner_html("");
            wcommands(node::Command::FetchMessages(chat, true));
        };
        let selected = move || current_chat.get() == Some(chat);
        let not_selected = move || current_chat.get() != Some(chat);
        view! { <div class="sb tac bp toe" class:hc=selected class:hov=not_selected on:click=select_chat> {chat.to_string()} </div> }
    };

    let cc_hidden = create_rw_signal(true);
    let cc_bts_disabled = create_rw_signal(false);
    let cc_name_input = create_node_ref::<Input>();
    let on_open_cc = move |_| cc_hidden.set(false);
    let on_close_cc = move |_| cc_hidden.set(true);
    let on_cc = move |_| {
        let Ok(chat) = ChatName::try_from(crate::get_value(cc_name_input).as_str()) else {
            return;
        };
        if chats.with(|chats| chats.contains(&chat)) {
            crate::report_validity(
                cc_name_input,
                format_args!("chat '{chat}' already exists and you have it"),
            );
            return;
        }
        wcommands(node::Command::CreateChat(chat));
        cc_bts_disabled.set(true);
    };

    let mi_hidden = create_rw_signal(true);
    let mi_bts_disabled = create_rw_signal(false);
    let mi_name_input = create_node_ref::<Input>();
    let on_open_mi = move |_| mi_hidden.set(false);
    let on_close_mi = move |_| mi_hidden.set(true);
    let on_mi = move |_| {
        let Ok(name) = UserName::try_from(crate::get_value(mi_name_input).as_str()) else {
            return;
        };

        let Some(chat) = current_chat.get_untracked() else {
            return;
        };

        mi_bts_disabled.set(true);
        spawn_local(async move {
            let client = crate::chain_node(rusername.get_untracked()).await.unwrap();
            let user = match client
                .get_profile_by_name(crate::user_contract(), name)
                .await
            {
                Ok(u) => UserIdentity::from(u).to_data(name),
                Err(e) => {
                    crate::report_validity(
                        mi_name_input,
                        format_args!("failed to fetch user: {e}"),
                    );
                    return;
                }
            };
            wcommands(node::Command::InviteUser { chat, user });
        })
    };

    create_effect(move |_| match revents() {
        node::Event::ChatCreated(chat) if chats.with_untracked(|chats| chats.contains(&chat)) => {}
        node::Event::ChatCreated(chat) => {
            chats.update(|chats| chats.push(chat));
            cc_hidden.set(true);
            cc_bts_disabled.set(false);
        }
        node::Event::CannotCreateChat(CreateChatErrorData { err, name }) => {
            crate::report_validity(
                cc_name_input,
                format_args!("failed to create '{name}': {err}"),
            );
            cc_bts_disabled.set(false);
        }
        node::Event::NewMessage {
            chat,
            name,
            content,
        } if current_chat.get_untracked() == Some(chat) => append_message(name, content),
        node::Event::FetchedMessages {
            chat,
            messages,
            end,
        } if current_chat.get_untracked() == Some(chat) => {
            red_all_messages.update(|v| *v |= end);
            for (name, content) in messages {
                prepend_message(name, content);
            }
        }
        node::Event::FetchedMessages { chat, messages, .. } => {
            log::info!("fetched messages for {chat}: {messages:#?}");
        }
        node::Event::MailWritten => {
            mi_hidden.set(true);
            mi_bts_disabled.set(false);
        }
        node::Event::MailWriteError(e) => {
            crate::report_validity(mi_name_input, format_args!("failed to write mail: {e}"));
            mi_bts_disabled.set(false);
        }
        node::Event::AddedMember(AddMember { .. }) => {
            mi_hidden.set(true);
            mi_bts_disabled.set(false);
        }
        _ => {}
    });

    let message_input = create_node_ref::<Input>();
    let on_input = move |e: web_sys::KeyboardEvent| {
        if e.key_code() != '\r' as u32 || e.get_modifier_state("Shift") {
            return;
        }

        let chat = current_chat.get_untracked().expect("universe to work");
        log::info!("sending message to {chat}");

        let content = crate::get_value(message_input);
        if content.is_empty() {
            return;
        }

        log::info!("sending message: {}", content);

        wcommands(node::Command::SendMessage { chat, content });
        message_input.get_untracked().unwrap().set_value("");
    };

    let message_scroll = create_node_ref::<leptos::html::Div>();
    let on_scroll = move |_| {
        if red_all_messages.get_untracked() {
            return;
        }

        let Some(chat) = current_chat.get_untracked() else {
            return;
        };

        if !is_at_bottom(message_scroll.get_untracked().unwrap()) {
            return;
        }

        wcommands(node::Command::FetchMessages(chat, false))
    };

    let chats_view =
        move || chats.with(|chats| chats.iter().map(|&chat| side_chat(chat)).collect_view());

    view! {
        <crate::Nav rusername/>
        <main class="tbm flx fg1 jcsb">
            <div class="sidebar bhc fg0 rbm oys pr">
                <div class="pa">
                    <div class="bp lsp sc sb tac">
                        "chats" <button class="hov sf pc lsm" on:click=on_open_cc >+</button>
                    </div>
                    {chats_view}
                    <div class="tac bm" hidden=move || !chats.with(Vec::is_empty)>"no chats yet"</div>
                </div>
            </div>
            <div class="sc fg1 flx pb fdc" hidden=move || current_chat.with(Option::is_none)>
                <div class="fg0 flx bm jcfe">
                    <button class="fg0 hov pc" on:click=on_open_mi>+</button>
                </div>
                <div class="fg1 flx fdc sc pr oys fy" on:scroll=on_scroll node_ref=message_scroll>
                    <div class="fg1 flx fdc bp sc fsc fy boa" node_ref=messages>
                    </div>
                </div>
                <input class="fg0 flx bm bp pc sf" type="text" placeholder="mesg..." node_ref=message_input on:keyup=on_input />
            </div>
            <div class="sc fg1 flx pb fdc" hidden=move || current_chat.with(Option::is_some)>
                <div class="ma">"no chat selected"</div>
            </div>
        </main>
        <div class="fsc flx blr sb" hidden=cc_hidden>
            <div class="sc flx fdc bp ma bsha">
                <input class="pc hov bp" type="text" placeholder="chat name..." maxlength=32 required node_ref=cc_name_input />
                <input class="pc hov bp tbm" type="button" value="create" disabled=cc_bts_disabled on:click=on_cc  />
                <input class="pc hov bp tbm" type="button" value="cancel" disabled=cc_bts_disabled on:click=on_close_cc />
            </div>
        </div>
        <div class="fsc flx blr sb" hidden=mi_hidden>
            <div class="sc flx fdc bp ma bsha">
                <input class="pc hov bp" type="text" placeholder="user to invite..." maxlength=32 required node_ref=mi_name_input />
                <input class="pc hov bp tbm" type="button" value="invite" disabled=cc_bts_disabled on:click=on_mi  />
                <input class="pc hov bp tbm" type="button" value="cancel" disabled=cc_bts_disabled on:click=on_close_mi />
            </div>
        </div>
    }.into_view()
}
