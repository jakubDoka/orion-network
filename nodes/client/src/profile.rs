use leptos::*;
use leptos_router::Redirect;

use crate::node::{self, Theme};

#[component]
pub fn Profile(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState {
        rkeys,
        rusername,
        wcommands,
        ..
    } = state;

    let Some(_keys) = rkeys.get_untracked() else {
        return view! { <Redirect path="/login"/> }.into_view();
    };

    let colors = Theme::KEYS;
    let style = web_sys::window()
        .unwrap()
        .get_computed_style(&document().body().unwrap())
        .unwrap()
        .unwrap();
    let elements = colors.iter().map(|&c| {
        let mut value = style.get_property_value(&format!("--{c}-color")).unwrap();
        value.truncate(7);
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
        let them = Theme::from_current();
        wcommands(node::Command::SetTheme(them));
    };

    view! {
        <crate::Nav rusername />
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
