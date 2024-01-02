use {
    crate::{
        handle_js_err, handled_async_callback, handled_async_closure, RawUserKeys, State, UserKeys,
    },
    anyhow::Context,
    chat_logic::{username_to_raw, UserName},
    crypto::TransmutationCircle,
    leptos::{html::Input, *},
    leptos_router::A,
    web_sys::js_sys::{Array, Uint8Array},
};

#[component]
pub fn Login(state: State) -> impl IntoView {
    let key_file = create_node_ref::<Input>();
    let on_login = handled_async_callback("logging in", move |_| async move {
        let file = key_file.get_untracked().expect("universe to work");
        file.set_custom_validity("");
        let Some(file_fut) = crate::load_file(file.clone()) else {
            anyhow::bail!("file not selected");
        };

        let bytes = file_fut
            .await
            .map_err(handle_js_err)
            .context("failed to load file")?;

        let Some(keys) = RawUserKeys::try_from_slice(&bytes) else {
            anyhow::bail!(
                "file is of incorrect size: {} != {}",
                bytes.len(),
                core::mem::size_of::<RawUserKeys>(),
            );
        };

        let Some(keys) = UserKeys::try_from_raw(keys.clone()) else {
            anyhow::bail!("invalid username");
        };

        state.keys.set(Some(keys));
        Ok(())
    });

    view! {
        <div class="sc flx fdc bp ma">
            <Nav/>
            <div class="flx fdc">
                <input class="pc hov bp tbm" type="file" style:width="250px"
                    node_ref=key_file on:change=on_login required />
            </div>
        </div>
    }
}

#[component]
pub fn Register(state: State) -> impl IntoView {
    let username = create_node_ref::<Input>();
    let download_link = create_node_ref::<leptos::html::A>();

    let on_register = handled_async_closure("registering", move || async move {
        let username = username.get_untracked().expect("universe to work");
        let username_content = UserName::try_from(username.value().as_str())
            .ok()
            .context("invalid username")?;

        let key = UserKeys::new(username_content);

        let client = crate::chain::node(username_content)
            .await
            .context("chain is not reachable")?;

        if client
            .user_exists(
                crate::chain::user_contract(),
                username_to_raw(username_content),
            )
            .await
            .context("user contract call failed")?
        {
            anyhow::bail!("user with this name already exists");
        }

        let key_bytes = key.clone().into_raw().into_bytes();
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

        client
            .register(
                crate::chain::user_contract(),
                username_to_raw(username_content),
                key.to_identity(),
                0,
            )
            .await
            .context("failed to create user")?;

        state.keys.set(Some(key));
        Ok(())
    });

    let validation_trigger = create_trigger();
    let on_change = move |e: web_sys::KeyboardEvent| {
        validation_trigger.notify();
        if e.key_code() == '\r' as u32 {
            on_register();
        }
    };
    let disabled = move || {
        validation_trigger.track();
        !username.get_untracked().unwrap().check_validity()
    };

    view! {
        <div class="sc flx fdc bp ma">
            <Nav/>
            <div class="flx fdc">
                <input class="pc hov bp tbm" type="text" placeholder="new username" maxlength="32"
                    node_ref=username on:keyup=on_change required />
                <input class="pc hov bp tbm" type="button" value="register"
                    on:click=move |_| on_register() disabled=disabled />
                <a hidden=true class="pc hov bp tbm bf tac"
                    node_ref=download_link>/download-again</a>
            </div>
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
