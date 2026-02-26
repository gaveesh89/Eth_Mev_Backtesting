//! Results page — summary cards, opportunities table, block breakdown.

use wasm_bindgen::JsCast;
use yew::prelude::*;

use crate::types::{AnalysisResults, MevType};

#[derive(Properties, PartialEq)]
pub struct ResultsProps {
    /// The analysis output.
    pub results: AnalysisResults,
    /// Go back to setup.
    pub on_reset: Callback<()>,
}

// We need PartialEq for AnalysisResults used in props
impl PartialEq for AnalysisResults {
    fn eq(&self, other: &Self) -> bool {
        self.blocks_analyzed == other.blocks_analyzed
            && self.transactions_analyzed == other.transactions_analyzed
            && self.opportunities.len() == other.opportunities.len()
    }
}

#[function_component(Results)]
pub fn results(props: &ResultsProps) -> Html {
    let r = &props.results;

    let arb_count = r
        .opportunities
        .iter()
        .filter(|o| o.mev_type == MevType::Arbitrage)
        .count();
    let sand_count = r
        .opportunities
        .iter()
        .filter(|o| o.mev_type == MevType::Sandwich)
        .count();
    let total_profit: f64 = r.opportunities.iter().map(|o| o.estimated_profit_eth).sum();
    let misordered = r
        .ordering_analysis
        .iter()
        .filter(|o| o.is_misordered)
        .count();

    // Tab state
    let active_tab = use_state(|| "opportunities".to_string());

    let set_tab = |tab: &'static str| {
        let active_tab = active_tab.clone();
        Callback::from(move |_: MouseEvent| {
            active_tab.set(tab.to_string());
        })
    };

    let on_reset = {
        let cb = props.on_reset.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    let on_export = {
        let results = r.clone();
        Callback::from(move |_: MouseEvent| {
            if let Ok(json) = serde_json::to_string_pretty(&results) {
                download_json(&json, "mev-results.json");
            }
        })
    };

    html! {
        <div class="results">
            // --- Summary cards ---
            <div class="summary-grid">
                <div class="summary-card">
                    <span class="summary-card__value">{ r.blocks_analyzed }</span>
                    <span class="summary-card__label">{"Blocks Analyzed"}</span>
                </div>
                <div class="summary-card">
                    <span class="summary-card__value">{ r.transactions_analyzed }</span>
                    <span class="summary-card__label">{"Transactions Scanned"}</span>
                </div>
                <div class="summary-card summary-card--accent">
                    <span class="summary-card__value">{ r.opportunities.len() }</span>
                    <span class="summary-card__label">{"MEV Opportunities"}</span>
                </div>
                <div class="summary-card">
                    <span class="summary-card__value">{ format!("{total_profit:.4}") }</span>
                    <span class="summary-card__label">{"Est. Profit (ETH)"}</span>
                </div>
            </div>

            // --- Type breakdown ---
            <div class="breakdown">
                <span class="tag tag--arb">{ format!("Arb: {arb_count}") }</span>
                <span class="tag tag--sand">{ format!("Sandwich: {sand_count}") }</span>
                <span class="tag tag--order">{ format!("Misordered: {misordered}") }</span>
            </div>

            // --- Tabs ---
            <div class="tabs">
                <button
                    class={classes!("tab", (*active_tab == "opportunities").then_some("tab--active"))}
                    onclick={set_tab("opportunities")}
                >
                    { format!("Opportunities ({})", r.opportunities.len()) }
                </button>
                <button
                    class={classes!("tab", (*active_tab == "ordering").then_some("tab--active"))}
                    onclick={set_tab("ordering")}
                >
                    { format!("Ordering ({misordered} misordered)") }
                </button>
                <button
                    class={classes!("tab", (*active_tab == "blocks").then_some("tab--active"))}
                    onclick={set_tab("blocks")}
                >
                    { format!("Blocks ({})", r.block_summaries.len()) }
                </button>
            </div>

            // --- Tab content ---
            <div class="tab-content">
                if *active_tab == "opportunities" {
                    { render_opportunities_table(r) }
                }
                if *active_tab == "ordering" {
                    { render_ordering_table(r) }
                }
                if *active_tab == "blocks" {
                    { render_blocks_table(r) }
                }
            </div>

            // --- Actions ---
            <div class="actions">
                <button class="btn btn--primary" onclick={on_export}>
                    {"📥 Export JSON"}
                </button>
                <button class="btn btn--secondary" onclick={on_reset}>
                    {"← New Analysis"}
                </button>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Sub-renders
// ---------------------------------------------------------------------------

fn render_opportunities_table(r: &AnalysisResults) -> Html {
    if r.opportunities.is_empty() {
        return html! {
            <div class="empty">
                <p>{"No MEV opportunities detected in this block range."}</p>
                <p class="empty__hint">{"Try a wider range or the USDC Depeg preset for higher MEV activity."}</p>
            </div>
        };
    }

    html! {
        <div class="table-wrap">
            <table class="table">
                <thead>
                    <tr>
                        <th>{"#"}</th>
                        <th>{"Block"}</th>
                        <th>{"Tx Hash"}</th>
                        <th>{"Type"}</th>
                        <th>{"Est. Profit"}</th>
                        <th>{"Confidence"}</th>
                        <th>{"Details"}</th>
                    </tr>
                </thead>
                <tbody>
                    { for r.opportunities.iter().enumerate().map(|(i, opp)| {
                        let type_class = match opp.mev_type {
                            MevType::Arbitrage => "tag--arb",
                            MevType::Sandwich => "tag--sand",
                            MevType::Liquidation => "tag--liq",
                            MevType::Unknown => "tag--unk",
                        };
                        let tx_short = if opp.tx_hash.len() > 12 {
                            format!("{}…{}", &opp.tx_hash[..8], &opp.tx_hash[opp.tx_hash.len()-6..])
                        } else {
                            opp.tx_hash.clone()
                        };
                        let etherscan = format!("https://etherscan.io/tx/{}", opp.tx_hash);
                        let conf_pct = format!("{:.0}%", opp.confidence * 100.0);

                        html! {
                            <tr>
                                <td class="cell--dim">{ i + 1 }</td>
                                <td class="cell--mono">{ opp.block_number }</td>
                                <td>
                                    <a class="cell--link" href={etherscan} target="_blank" rel="noopener">
                                        { tx_short }
                                    </a>
                                </td>
                                <td><span class={classes!("tag", type_class)}>{ &opp.mev_type.to_string() }</span></td>
                                <td class="cell--mono">{ format!("{:.6}", opp.estimated_profit_eth) }</td>
                                <td>{ &conf_pct }</td>
                                <td class="cell--detail">{ &opp.details }</td>
                            </tr>
                        }
                    })}
                </tbody>
            </table>
        </div>
    }
}

fn render_ordering_table(r: &AnalysisResults) -> Html {
    let misordered: Vec<_> = r
        .ordering_analysis
        .iter()
        .filter(|o| o.is_misordered)
        .collect();

    if misordered.is_empty() {
        return html! {
            <div class="empty">
                <p>{"All transactions appear to be ordered by EGP — no misordering detected."}</p>
            </div>
        };
    }

    html! {
        <div class="table-wrap">
            <table class="table">
                <thead>
                    <tr>
                        <th>{"Block"}</th>
                        <th>{"Tx Hash"}</th>
                        <th>{"Actual Pos"}</th>
                        <th>{"Optimal Pos"}</th>
                        <th>{"Δ"}</th>
                        <th>{"EGP (Gwei)"}</th>
                    </tr>
                </thead>
                <tbody>
                    { for misordered.iter().map(|o| {
                        let delta = o.actual_position as i64 - o.optimal_position as i64;
                        let delta_class = if delta > 0 { "cell--negative" } else { "cell--positive" };
                        let tx_short = if o.tx_hash.len() > 12 {
                            format!("{}…{}", &o.tx_hash[..8], &o.tx_hash[o.tx_hash.len()-6..])
                        } else {
                            o.tx_hash.clone()
                        };
                        html! {
                            <tr>
                                <td class="cell--mono">{ o.block_number }</td>
                                <td class="cell--mono">{ tx_short }</td>
                                <td>{ o.actual_position }</td>
                                <td>{ o.optimal_position }</td>
                                <td class={delta_class}>{ format!("{delta:+}") }</td>
                                <td class="cell--mono">{ format!("{:.2}", o.egp_gwei) }</td>
                            </tr>
                        }
                    })}
                </tbody>
            </table>
        </div>
    }
}

fn render_blocks_table(r: &AnalysisResults) -> Html {
    html! {
        <div class="table-wrap">
            <table class="table">
                <thead>
                    <tr>
                        <th>{"Block"}</th>
                        <th>{"Txns"}</th>
                        <th>{"Gas Used"}</th>
                        <th>{"Base Fee (Gwei)"}</th>
                        <th>{"Block Value (ETH)"}</th>
                        <th>{"MEV Found"}</th>
                    </tr>
                </thead>
                <tbody>
                    { for r.block_summaries.iter().map(|b| {
                        let gas_pct = (b.gas_used as f64 / 30_000_000.0 * 100.0).min(100.0);
                        html! {
                            <tr>
                                <td class="cell--mono">{ b.block_number }</td>
                                <td>{ b.tx_count }</td>
                                <td>
                                    <div class="gas-bar-wrap">
                                        <div class="gas-bar" style={format!("width: {gas_pct:.0}%")} />
                                        <span>{ format!("{:.1}%", gas_pct) }</span>
                                    </div>
                                </td>
                                <td class="cell--mono">{ format!("{:.2}", b.base_fee_gwei) }</td>
                                <td class="cell--mono">{ format!("{:.6}", b.block_value_eth) }</td>
                                <td>
                                    if b.mev_count > 0 {
                                        <span class="tag tag--arb">{ b.mev_count }</span>
                                    } else {
                                        <span class="cell--dim">{"0"}</span>
                                    }
                                </td>
                            </tr>
                        }
                    })}
                </tbody>
            </table>
        </div>
    }
}

// ---------------------------------------------------------------------------
// JSON export helper
// ---------------------------------------------------------------------------

fn download_json(json: &str, filename: &str) {
    use js_sys::Array;
    use wasm_bindgen::JsValue;
    use web_sys::{Blob, BlobPropertyBag, Url};

    let arr = Array::new();
    arr.push(&JsValue::from_str(json));

    let opts = BlobPropertyBag::new();
    opts.set_type("application/json");

    if let Ok(blob) = Blob::new_with_str_sequence_and_options(&arr, &opts) {
        if let Ok(url) = Url::create_object_url_with_blob(&blob) {
            let window = web_sys::window().expect("no window");
            let document = window.document().expect("no document");
            if let Ok(a) = document.create_element("a") {
                let anchor: web_sys::HtmlAnchorElement = a.unchecked_into();
                anchor.set_href(&url);
                anchor.set_download(filename);
                anchor.click();
                let _ = Url::revoke_object_url(&url);
            }
        }
    }
}
