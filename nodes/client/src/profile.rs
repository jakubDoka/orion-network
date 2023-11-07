use leptos::*;
use leptos_router::Redirect;

#[component]
pub fn Profile(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState {
        rkeys, rusername, ..
    } = state;

    let Some(_keys) = rkeys.get_untracked() else {
        return view! { <Redirect path="/login"/> }.into_view();
    };

    let colors = ["primary", "secondary", "highlight", "font", "error"];
    let style = web_sys::window()
        .unwrap()
        .get_computed_style(&document().body().unwrap())
        .unwrap()
        .unwrap();
    let elements = colors.map(|c| {
        let value = style.get_property_value(&format!("--{c}-color")).unwrap();
        let on_change = move |e: web_sys::Event| {
            let value = event_target_value(&e);
            document()
                .body()
                .unwrap()
                .style()
                .set_property(&format!("--{c}-color"), &value)
                .unwrap();
        };
        view! {
            <div class="flx tbm">
                <input type="color" name=c value=value oninput="updateTheme()" on:chnage=on_change />
                <span class="lbp">{c}</span>
            </div>
        }
    });

    view! {
        <crate::Nav rusername />
        <main class="tbm fg1 sc bp">
            <div class="flx">
                <form id="theme-form" class="flx fg0 fdc bp pc">
                    <span class="lbp">theme</span>
                    {elements.into_iter().collect_view()}
                    <input class="sc hov bp sf tbm" type="button" value="save" />
                </form>
            </div>
        </main>
    }
    .into_view()
}
