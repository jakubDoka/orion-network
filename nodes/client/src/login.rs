use leptos::html::Input;
use leptos::*;
use leptos_router::A;
use protocols::chat::{SerializedUserKeys, UserKeys, UserName, USER_KEYS_SIZE};
use protocols::contracts::UserData;
use web_sys::js_sys::{Array, Uint8Array};

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
        <div class="sc flx fdc bp ma">
            <Nav/>
            <form class="flx fdc">
                <input class="pc hov bp tbm" type="file" node_ref=key_file on:change=on_change required />
            </form>
        </div>
    }
}

#[component]
pub fn Register(wkeys: WriteSignal<Option<UserKeys>>) -> impl IntoView {
    let username = create_node_ref::<Input>();
    let download_link = create_node_ref::<leptos::html::A>();

    let on_register = move |_| {
        let username = username.get_untracked().expect("universe to work");
        let Ok(username_content) = UserName::try_from(username.value().as_str()) else {
            return;
        };
        let key = UserKeys::new(username_content);

        spawn_local(async move {
            let client = crate::chain_node(username_content).await.unwrap();

            if client
                .get_profile_by_name(crate::user_contract(), username_content)
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

            if let Err(e) = client.register(crate::user_contract(), data).await {
                username.set_custom_validity(&format!("failed to create user: {e:?}"));
                username.report_validity();
                return;
            }

            wkeys(Some(key_bytes.into()));
        });
    };

    let validation_trigger = create_trigger();
    let on_change = move |_| validation_trigger.notify();
    let disabled = move || {
        validation_trigger.track();
        !username.get_untracked().unwrap().check_validity()
    };

    view! {
        <div class="sc flx fdc bp ma">
            <Nav/>
            <form class="flx fdc">
                <input class="pc hov bp tbm" type="text" placeholder="new username" maxlength="32" node_ref=username on:input=on_change required />
                <input class="pc hov bp tbm" type="button" value="register" on:click=on_register disabled=disabled />
                <a hidden=true class="pc hov bp tbm bf tac" node_ref=download_link>/download-again</a>
            </form>
        </div>
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
