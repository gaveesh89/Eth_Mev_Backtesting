//! Setup page — configuration form for RPC, block range, and strategy.

use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::types::{AppConfig, Strategy, PRESETS};

#[derive(Properties, PartialEq)]
pub struct SetupProps {
    /// Fires when the user clicks "Start Analysis".
    pub on_start: Callback<AppConfig>,
    /// Optional error message to display at the top.
    #[prop_or_default]
    pub error: Option<AttrValue>,
    /// Whether the connect-test is in progress.
    #[prop_or(false)]
    pub testing: bool,
}

#[function_component(Setup)]
pub fn setup(props: &SetupProps) -> Html {
    let provider = use_state(|| "alchemy".to_string());
    let api_key = use_state(String::new);
    let custom_url = use_state(String::new);
    let start_block = use_state(|| "16817000".to_string());
    let end_block = use_state(|| "16817009".to_string());
    let strategy = use_state(|| Strategy::FullMevScan);

    // --- Input handlers ---
    let on_provider = {
        let provider = provider.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            provider.set(select.value());
        })
    };

    let on_api_key = {
        let api_key = api_key.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            api_key.set(input.value());
        })
    };

    let on_custom_url = {
        let custom_url = custom_url.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            custom_url.set(input.value());
        })
    };

    let on_start_block = {
        let start_block = start_block.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            start_block.set(input.value());
        })
    };

    let on_end_block = {
        let end_block = end_block.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            end_block.set(input.value());
        })
    };

    let on_strategy = {
        let strategy = strategy.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let val = match select.value().as_str() {
                "dex_dex" => Strategy::DexDexArb,
                "sandwich" => Strategy::SandwichDetection,
                _ => Strategy::FullMevScan,
            };
            strategy.set(val);
        })
    };

    // Preset buttons
    let preset_buttons = PRESETS.iter().map(|p| {
        let start_block = start_block.clone();
        let end_block = end_block.clone();
        let s = p.start;
        let e = p.end;
        let label = p.label;
        let desc = p.description;
        let onclick = Callback::from(move |_: MouseEvent| {
            start_block.set(s.to_string());
            end_block.set(e.to_string());
        });
        html! {
            <button
                class="preset-btn"
                onclick={onclick}
                title={desc}
                type="button"
            >
                { label }
            </button>
        }
    });

    // Submit handler
    let on_submit = {
        let provider = provider.clone();
        let api_key = api_key.clone();
        let custom_url = custom_url.clone();
        let start_block = start_block.clone();
        let end_block = end_block.clone();
        let strategy = strategy.clone();
        let on_start = props.on_start.clone();

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();

            let rpc_url = match (*provider).as_str() {
                "alchemy" => {
                    format!("https://eth-mainnet.g.alchemy.com/v2/{}", (*api_key).trim())
                }
                "infura" => {
                    format!("https://mainnet.infura.io/v3/{}", (*api_key).trim())
                }
                _ => (*custom_url).trim().to_string(),
            };

            let sb: u64 = (*start_block).parse().unwrap_or(16_817_000);
            let eb: u64 = (*end_block).parse().unwrap_or(16_817_009);

            on_start.emit(AppConfig {
                rpc_url,
                start_block: sb,
                end_block: eb,
                strategy: (*strategy).clone(),
            });
        })
    };

    let show_api_key = *provider == "alchemy" || *provider == "infura";

    // Compute block count hint outside html! macro (turbofish/parse not allowed inside)
    let block_hint = {
        let sb = (*start_block).parse::<u64>().unwrap_or(0);
        let eb = (*end_block).parse::<u64>().unwrap_or(0);
        let count = if eb >= sb { eb - sb + 1 } else { 0 };
        format!("{count} block(s) — ~{} RPC calls", count * 2)
    };

    html! {
        <form class="setup" onsubmit={on_submit}>

            // Error banner
            if let Some(err) = &props.error {
                <div class="alert alert--error">
                    <span class="alert__icon">{"⚠"}</span>
                    { err }
                </div>
            }

            // --- RPC Provider Card ---
            <div class="card">
                <h2 class="card__title">{"🔗 RPC Provider"}</h2>
                <p class="card__desc">
                    {"Connect to an Ethereum archive node. Alchemy and Infura both offer free tiers."}
                </p>

                <label class="field">
                    <span class="field__label">{"Provider"}</span>
                    <select class="field__select" onchange={on_provider} value={(*provider).clone()}>
                        <option value="alchemy" selected={*provider == "alchemy"}>{"Alchemy"}</option>
                        <option value="infura" selected={*provider == "infura"}>{"Infura"}</option>
                        <option value="custom" selected={*provider == "custom"}>{"Custom URL"}</option>
                    </select>
                </label>

                if show_api_key {
                    <label class="field">
                        <span class="field__label">{"API Key"}</span>
                        <input
                            class="field__input"
                            type="password"
                            placeholder="Paste your API key…"
                            value={(*api_key).clone()}
                            oninput={on_api_key}
                        />
                    </label>
                } else {
                    <label class="field">
                        <span class="field__label">{"RPC URL"}</span>
                        <input
                            class="field__input"
                            type="url"
                            placeholder="https://your-node.example.com"
                            value={(*custom_url).clone()}
                            oninput={on_custom_url}
                        />
                    </label>
                }
            </div>

            // --- Block Range Card ---
            <div class="card">
                <h2 class="card__title">{"📦 Block Range"}</h2>
                <p class="card__desc">
                    {"Select a preset or enter a custom range. Each block requires 2 RPC calls."}
                </p>

                <div class="preset-grid">
                    { for preset_buttons }
                </div>

                <div class="field-row">
                    <label class="field">
                        <span class="field__label">{"Start Block"}</span>
                        <input
                            class="field__input field__input--mono"
                            type="number"
                            value={(*start_block).clone()}
                            oninput={on_start_block}
                            min="0"
                        />
                    </label>
                    <label class="field">
                        <span class="field__label">{"End Block"}</span>
                        <input
                            class="field__input field__input--mono"
                            type="number"
                            value={(*end_block).clone()}
                            oninput={on_end_block}
                            min="0"
                        />
                    </label>
                </div>

                <p class="card__hint">
                    { block_hint.clone() }
                </p>
            </div>

            // --- Strategy Card ---
            <div class="card">
                <h2 class="card__title">{"🎯 Analysis Strategy"}</h2>
                <p class="card__desc">
                    {"Choose which MEV patterns to scan for."}
                </p>

                <label class="field">
                    <span class="field__label">{"Strategy"}</span>
                    <select class="field__select" onchange={on_strategy}>
                        <option value="full" selected={*strategy == Strategy::FullMevScan}>
                            {"Full MEV Scan — Arbitrage + Sandwich"}
                        </option>
                        <option value="dex_dex" selected={*strategy == Strategy::DexDexArb}>
                            {"DEX-DEX Arbitrage Only"}
                        </option>
                        <option value="sandwich" selected={*strategy == Strategy::SandwichDetection}>
                            {"Sandwich Detection Only"}
                        </option>
                    </select>
                </label>
            </div>

            // --- Submit ---
            <button class="btn btn--primary btn--lg" type="submit" disabled={props.testing}>
                if props.testing {
                    <span class="spinner" />
                    {" Testing connection…"}
                } else {
                    {"▶ Start Analysis"}
                }
            </button>
        </form>
    }
}
