use leptos::*;
use leptos_router::Redirect;

#[leptos::component]
pub fn Chat(state: crate::LoggedState) -> impl IntoView {
    let crate::LoggedState {
        revents,
        wcommands,
        rkeys,
        rusername,
    } = state;

    let Some(_keys) = rkeys.get_untracked() else {
        return view! { <Redirect path="/login"/> };
    };

    view! {
        <crate::Nav rusername/>
        <main class="tbm flx fg1 jcsb">
            <div class="sidebar bhc fg0 rbm oys pr">
                <div class="pa">
                    <div class="bp lsp sc sb tac">
                        rooms
                    </div>
                    <div class="sb hov tac bp toe">
                        some room
                    </div>

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
                </div>
            </div>
            <div class="sc fg1 flx pb fdc">
                <div class="fg1">

                </div>
                <div class="fg0 flx bm bp pc">
                    <input class="fg1 rsb sc hov" type="text" placeholder="mesg..." />
                    <svg class="sc lsb hov" xmlns="http://www.w3.org/2000/svg" height="31" viewBox="0 -960 960 960" width="30">
                        <path d="M120-160v-640l760 320-760 320Zm80-120 474-200-474-200v140l240 60-240 60v140Zm0 0v-400 400Z" />
                    </svg>
                </div>
            </div>
        </main>
    }.into()
}
