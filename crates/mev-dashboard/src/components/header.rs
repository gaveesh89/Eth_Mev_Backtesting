//! Top navigation / header bar.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct HeaderProps {
    /// Current step label for the breadcrumb.
    pub step: AttrValue,
}

#[function_component(Header)]
pub fn header(props: &HeaderProps) -> Html {
    html! {
        <header class="header">
            <div class="header__brand">
                <span class="header__icon">{"⛓"}</span>
                <h1 class="header__title">{"MEV Backtest Dashboard"}</h1>
                <span class="header__badge">{"WASM"}</span>
            </div>
            <div class="header__meta">
                <span class="header__step">{ &props.step }</span>
                <a
                    class="header__github"
                    href="https://github.com/gaveesh89/Eth_Mev_Backtesting"
                    target="_blank"
                    rel="noopener noreferrer"
                >
                    {"GitHub ↗"}
                </a>
            </div>
        </header>
    }
}
