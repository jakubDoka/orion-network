use component_utils::Reminder;

use {chat_logic::SetVault, component_utils::Codec};

use {crate::node::Theme, leptos::*, leptos_router::Redirect};

#[component]
pub fn Profile(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState { user_state } = state;

    let Some(keys) = user_state.with_untracked(|us| us.keys.clone()) else {
        return view! { <Redirect path="/login"/> }.into_view();
    };
    let my_name = keys.name;
    let identity = crypto::hash::new(&keys.sign.public_key());
    let ed = move || user_state.with_untracked(|us| us.requests.clone()).unwrap();

    let colors = Theme::KEYS;
    let style = web_sys::window()
        .unwrap()
        .get_computed_style(&document().body().unwrap())
        .unwrap()
        .unwrap();
    let elements = colors.iter().map(|&c| {
        let mut value = style.get_property_value(&format!("--{c}-color")).unwrap();
        value.truncate("#000000".len());
        let on_change = move |e: web_sys::Event| {
            let value = event_target_value(&e);
            document()
                .body()
                .unwrap()
                .style()
                .set_property(&format!("--{c}-color"), &format!("{}ff", value))
                .unwrap();
        };
        view! {
            <div class="flx tbm">
                <input type="color" name=c value=value on:input=on_change />
                <span class="lbp">{c}</span>
            </div>
        }
    });

    let on_save = move |_| {
        let them = Theme::from_current().unwrap_or_default();
        user_state.update(|us| us.vault.theme = them);
        let mut vault_bytes = user_state.with_untracked(|us| us.vault.to_bytes());
        let secret = user_state.with_untracked(|us| us.keys.as_ref().unwrap().vault);
        let proof = user_state
            .try_update(|us| us.next_profile_proof())
            .unwrap()
            .unwrap();
        crypto::encrypt(&mut vault_bytes, secret);
        spawn_local(async move {
            ed().dispatch::<SetVault>(identity, (proof, Reminder(&vault_bytes)))
                .await
                .unwrap()
                .unwrap();
        });
    };

    view! {
        <crate::Nav my_name />
        <main class="tbm fg1 sc bp">
            <div class="flx">
                <form id="theme-form" class="flx fg0 fdc bp pc">
                    <span class="lbp">theme</span>
                    {elements.into_iter().collect_view()}
                    <input class="sc hov bp sf tbm" type="button" value="save" on:click=on_save />
                </form>
            </div>
        </main>
    }
    .into_view()
}
