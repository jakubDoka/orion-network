use core::fmt;
use std::future::Future;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};

use component_utils::DropFn;
use crypto::TransmutationCircle;
use leptos::html::{Input, Textarea};
use leptos::*;
use leptos_router::Redirect;
use primitives::chat::{AddMember, ChatName, CreateChatErrorData, UserName};
use primitives::contracts::{StoredUserIdentity, UserIdentity};

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
    let (show_chat, set_show_chat) = create_signal(false);
    let messages = create_node_ref::<leptos::html::Div>();

    let hide_chat = move |_| set_show_chat(false);
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
            set_show_chat(true);
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

    let (create_chat_button, create_chat_poppup) = popup(
        PoppupStyle {
            placeholder: "chat name...",
            button_style: "hov sf pc lsm",
            button: "+",
            confirm: "create",
            maxlength: 32,
        },
        move |chat| async move {
            let Ok(chat) = ChatName::try_from(chat.as_str()) else {
                return Err("invalid chat name".to_owned());
            };

            if chats.with(|chats| chats.contains(&chat)) {
                return Err("chat already exists, you are part of it".to_owned());
            }

            state
                .request(node::Command::CreateChat(chat), move |e| match e {
                    node::Event::ChatCreated(c) if c == chat => Some(Ok(())),
                    node::Event::CannotCreateChat(CreateChatErrorData { err, name })
                        if name == chat =>
                    {
                        Some(Err(err))
                    }
                    _ => None,
                })
                .await
                .map_err(|e| e.to_string())?;

            chats.update(|chats| chats.push(chat));

            Ok(())
        },
    );

    let (invite_user_button, invite_user_poppup) = popup(
        PoppupStyle {
            placeholder: "user to invite...",
            button_style: "fg0 hov pc",
            button: "+",
            confirm: "invite",
            maxlength: 32,
        },
        move |name| async move {
            static ADD_STAGE: AtomicUsize = AtomicUsize::new(0);
            if ADD_STAGE.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed) != Ok(0) {
                return Err("already adding user".to_owned());
            }
            DropFn::new(|| ADD_STAGE.store(0, Ordering::Relaxed));

            let Ok(name) = UserName::try_from(name.as_str()) else {
                return Err("invalid user name".to_owned());
            };

            let Some(chat) = current_chat.get_untracked() else {
                return Err("no chat selected".to_owned());
            };

            let client = crate::chain_node(rusername.get_untracked()).await.unwrap();
            let user = match client
                .get_profile_by_name(crate::user_contract(), name)
                .await
            {
                Ok(Some(u)) => StoredUserIdentity::from_bytes(u).to_data(name),
                Ok(None) => return Err(format!("user {name} does not exist")),
                Err(e) => return Err(format!("failed to fetch user: {e}")),
            };

            state
                .request(node::Command::InviteUser { chat, user }, move |e| match e {
                    node::Event::AddedMember(AddMember { invited, .. })
                        if user.sign == invited
                            && ADD_STAGE.fetch_add(1, Ordering::Relaxed) == 3 =>
                    {
                        Some(Ok(()))
                    }
                    node::Event::MailWritten if ADD_STAGE.fetch_add(1, Ordering::Relaxed) == 3 => {
                        Some(Ok(()))
                    }
                    node::Event::MailWriteError(e) => Some(Err(e.to_string())),
                    _ => None,
                })
                .await
        },
    );

    create_effect(move |_| match revents() {
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
        _ => {}
    });

    let message_input = create_node_ref::<Textarea>();
    let on_input = move |e: web_sys::KeyboardEvent| {
        let mi = message_input.get_untracked().unwrap();
        let outher_height = window()
            .get_computed_style(&mi)
            .unwrap()
            .unwrap()
            .get_property_value("height")
            .unwrap()
            .strip_suffix("px")
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let diff = outher_height - mi.client_height();
        mi.deref().style().set_property("height", "0px").unwrap();
        mi.deref()
            .style()
            .set_property(
                "height",
                format!("{}px", mi.scroll_height() + diff).as_str(),
            )
            .unwrap();

        if e.key_code() != '\r' as u32 || e.get_modifier_state("Shift") {
            return;
        }

        let chat = current_chat.get_untracked().expect("universe to work");

        let content = message_input.get_untracked().unwrap().value();
        if content.trim().is_empty() {
            return;
        }

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
    let chats_are_empty = move || chats.with(Vec::is_empty);
    let chat_selected = move || current_chat.with(Option::is_some);
    let get_chat = move || current_chat.get().unwrap_or_default().to_string();

    view! {
        <crate::Nav rusername/>
        <main id="main" class="tbm flx fg1 jcsb">
            <div id="sidebar" class="bhc fg0 rbm oys pr" class=("off-screen", show_chat)>
                <div class="pa w100">
                    <div class="bp lsp sc sb tac">
                        "chats"
                        {create_chat_button}
                    </div>
                    {chats_view}
                    <div class="tac bm" hidden=crate::not(chats_are_empty)>"no chats yet"</div>
                </div>
            </div>
            <div class="sc fg1 flx pb fdc" hidden=crate::not(chat_selected) class=("off-screen", crate::not(show_chat))>
                <div class="fg0 flx bm jcsb">
                    <button class="hov sf pc lsm phone-only" on:click=hide_chat>"<"</button>
                    <div class="phone-only">{get_chat}</div>
                    {invite_user_button}
                </div>
                <div class="fg1 flx fdc sc pr oys fy" on:scroll=on_scroll node_ref=message_scroll>
                    <div class="fg1 flx fdc bp sc fsc fy boa" node_ref=messages></div>
                </div>
                <textarea class="fg0 flx bm bp pc sf" type="text" rows=1 placeholder="mesg..."
                    node_ref=message_input on:keyup=on_input onresize="adjustHeight(this)"
                    onkeydown="adjustHeight(this)"/>
            </div>
            <div class="sc fg1 flx pb fdc" hidden=chat_selected class=("off-screen", crate::not(show_chat))>
                <div class="ma">"no chat selected"</div>
            </div>
        </main>
        {create_chat_poppup}
        {invite_user_poppup}
    }.into_view()
}

struct PoppupStyle {
    placeholder: &'static str,
    button_style: &'static str,
    button: &'static str,
    confirm: &'static str,
    maxlength: usize,
}

fn popup<E: fmt::Display, F: Future<Output = Result<(), E>>>(
    style: PoppupStyle,
    on_confirm: impl Fn(String) -> F + 'static + Copy,
) -> (impl IntoView, impl IntoView) {
    let (hidden, set_hidden) = create_signal(true);
    let (controls_disabled, set_controls_disabled) = create_signal(false);
    let input = create_node_ref::<Input>();
    let input_trigger = create_trigger();

    let show = move |_| {
        input.get_untracked().unwrap().focus().unwrap();
        set_hidden(false);
    };
    let close = move |_| set_hidden(true);
    let on_confirm = move || {
        let content = crate::get_value(input);
        if content.is_empty() || content.len() > style.maxlength {
            return;
        }

        spawn_local(async move {
            set_controls_disabled(true);
            match on_confirm(content).await {
                Ok(()) => set_hidden(true),
                Err(e) => crate::report_validity(input, e),
            }
            set_controls_disabled(false);
        })
    };
    let on_input = move |_| {
        input_trigger.notify();
        crate::report_validity(input, "")
    };
    let on_keydown = move |e: web_sys::KeyboardEvent| {
        if e.key_code() == '\r' as u32 && !e.get_modifier_state("Shift") {
            on_confirm();
        }

        if e.key_code() == 27 && !controls_disabled.get_untracked() {
            set_hidden(true);
        }
    };
    let confirm_disabled = move || {
        input_trigger.track();
        controls_disabled() || !input.get_untracked().unwrap().check_validity()
    };

    let button = view! { <button class=style.button_style on:click=show>{style.button}</button> };
    let popup = view! {
        <div class="fsc flx blr sb" hidden=hidden>
            <div class="sc flx fdc bp ma bsha">
                <input class="pc hov bp" type="text" placeholder=style.placeholder
                    maxlength=style.maxlength required node_ref=input on:click=move |_| on_confirm()
                    on:input=on_input on:keydown=on_keydown/>
                <input class="pc hov bp tbm" type="button" value=style.confirm disabled=confirm_disabled />
                <input class="pc hov bp tbm" type="button" value="cancel" disabled=controls_disabled on:click=close />
            </div>
        </div>
    };
    (button, popup)
}
