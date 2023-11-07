use chain_api::UserData;
use leptos::html::Input;
use leptos::*;
use leptos_router::A;
use protocols::chat::{SerializedUserKeys, UserKeys, UserName, USER_KEYS_SIZE};
use web_sys::js_sys::{Array, Uint8Array};

use crate::CHAIN_BOOTSTRAP_NODE;

#[component]
pub fn Login(wkeys: WriteSignal<Option<UserKeys>>) -> impl IntoView {
    let key_file = create_node_ref::<Input>();
    let on_change = move |_| {
        let file = key_file.get_untracked().expect("universe to work");
        let Some(file_fut) = crate::load_file(file.clone()) else {
            log::debug!("file removed");
            return;
        };

        spawn_local(async move {
            file.set_custom_validity("");
            let bytes = match file_fut.await {
                Ok(file) => file,
                Err(e) => {
                    file.set_custom_validity(&format!("failed to load file: {e:?}"));
                    file.report_validity();
                    return;
                }
            };

            let keys = match SerializedUserKeys::try_from(bytes) {
                Ok(keys) => keys,
                Err(e) => {
                    file.set_custom_validity(&format!(
                        "file is of incorrect size: {} != {}",
                        e.len(),
                        USER_KEYS_SIZE,
                    ));
                    file.report_validity();
                    return;
                }
            };

            wkeys(Some(keys.into()));
        });
    };

    view! {
        <Nav/>
        <form class="flx fdc">
            <input class="pc hov bp tbm" type="file" node_ref=key_file on:change=on_change required />
        </form>
    }
}

#[component]
pub fn Register(wkeys: WriteSignal<Option<UserKeys>>) -> impl IntoView {
    let username = create_node_ref::<Input>();
    let download_link = create_node_ref::<leptos::html::A>();

    let on_change = move |_| {};

    let on_register = move |_| {
        let key = UserKeys::new();
        let username = username.get_untracked().expect("universe to work");
        let Ok(username_content) = UserName::try_from(username.value().as_str()) else {
            username.set_custom_validity("username is too long");
            username.report_validity();
            return;
        };

        spawn_local(async move {
            if chain_api::user_by_name(CHAIN_BOOTSTRAP_NODE, &username_content)
                .await
                .is_ok()
            {
                username.set_custom_validity("username already exists");
                username.report_validity();
                return;
            }

            let data = UserData {
                name: username_content,
                enc: key.enc.public_key().into(),
                sign: key.sign.public_key().into(),
            };

            let key_bytes = SerializedUserKeys::from(key);
            let url = web_sys::Url::create_object_url_with_blob(
                &web_sys::Blob::new_with_u8_array_sequence_and_options(
                    &Array::from_iter(vec![Uint8Array::from(key_bytes.as_slice())]),
                    web_sys::BlobPropertyBag::new().type_("application/octet-stream"),
                )
                .unwrap(),
            )
            .unwrap();

            let link = download_link.get_untracked().expect("universe to work");
            link.set_hidden(false);
            link.set_href(&url);
            link.set_download(&format!("{}.keys", username_content));
            link.click();

            if let Err(e) = chain_api::create_user(CHAIN_BOOTSTRAP_NODE, data).await {
                username.set_custom_validity(&format!("failed to create user: {e:?}"));
                username.report_validity();
                return;
            }

            wkeys(Some(key_bytes.into()));
        });
    };

    view! {
        <Nav/>
        <form class="flx fdc">
            <input class="pc hov bp tbm" type="text" placeholder="new username" maxlength="32" node_ref=username required on:change=on_change />
            <input class="pc hov bp tbm" type="button" value="register" on:click=on_register />
            <a hidden=true class="pc hov bp tbm bf tac" node_ref=download_link>/download-again</a>
        </form>
    }
}

#[component]
fn Nav() -> impl IntoView {
    view! {
        <nav class="flx jcsb">
            <A class="bf hov bp rsb" href="/login">/login</A>
            <A class="bf hov bp sb" href="/register">/register</A>
        </nav>
    }
}
