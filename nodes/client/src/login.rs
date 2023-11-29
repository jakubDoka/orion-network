use {
    crate::{handle_js_err, RawUserKeys, State, UserKeys},
    anyhow::Context,
    crypto::TransmutationCircle,
    leptos::{html::Input, *},
    leptos_router::A,
    primitives::{contracts::UserData, UserName},
    web_sys::js_sys::{Array, Uint8Array},
};

#[component]
pub fn Login(state: State) -> impl IntoView {
    let key_file = create_node_ref::<Input>();
    let login = move || async move {
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
    };

    let on_change = move |_| {
        spawn_local(async move {
            if let Err(e) = login().await {
                crate::report_validity(key_file, format_args!("{e:#}"));
            }
        });
    };

    view! {
        <div class="sc flx fdc bp ma">
            <Nav/>
            <form class="flx fdc">
                <input class="pc hov bp tbm" type="file" style:width="250px"
                    node_ref=key_file on:change=on_change required />
            </form>
        </div>
    }
}

#[component]
pub fn Register(state: State) -> impl IntoView {
    let username = create_node_ref::<Input>();
    let download_link = create_node_ref::<leptos::html::A>();

    let register = move || async move {
        let username = username.get_untracked().expect("universe to work");
        let username_content = UserName::try_from(username.value().as_str())
            .ok()
            .context("invalid username")?;
        let key = UserKeys::new(username_content);

        let client = crate::chain::node(username_content)
            .await
            .context("chain is not reachable")?;

        if client
            .user_exists(crate::chain::user_contract(), username_content)
            .await
            .context("user contract call failed")?
        {
            anyhow::bail!("user with this name already exists");
        }

        let data = UserData {
            name: username_content,
            enc: key.enc.public_key(),
            sign: key.sign.public_key(),
        };

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
                username_content,
                data.to_identity().to_stored(),
            )
            .await
            .context("failed to create user")?;

        state.keys.set(Some(key));
        Ok(())
    };

    let on_register = move |_| {
        spawn_local(async move {
            if let Err(e) = register().await {
                crate::report_validity(username, format_args!("{e:#}"));
            }
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
