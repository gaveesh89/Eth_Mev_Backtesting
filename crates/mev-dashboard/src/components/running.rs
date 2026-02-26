//! Running page — progress bar and live log during analysis.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RunningProps {
    /// Progress fraction 0.0 ..= 1.0
    pub progress: f64,
    /// Current status message lines.
    pub log: Vec<AttrValue>,
    /// Blocks completed / total.
    pub blocks_done: u64,
    pub blocks_total: u64,
}

#[function_component(Running)]
pub fn running(props: &RunningProps) -> Html {
    let pct = (props.progress * 100.0).min(100.0);
    let pct_str = format!("{pct:.0}%");

    html! {
        <div class="running">
            <div class="card running__card">
                <h2 class="card__title">{"⏳ Analyzing Blocks…"}</h2>
                <p class="card__desc">
                    { format!(
                        "Block {}/{} — fetching data and scanning for MEV patterns",
                        props.blocks_done, props.blocks_total
                    )}
                </p>

                // Progress bar
                <div class="progress">
                    <div
                        class="progress__bar"
                        style={format!("width: {pct}%")}
                    />
                    <span class="progress__label">{ &pct_str }</span>
                </div>

                // Stats row
                <div class="running__stats">
                    <div class="stat">
                        <span class="stat__value">{ props.blocks_done }</span>
                        <span class="stat__label">{"Blocks Fetched"}</span>
                    </div>
                    <div class="stat">
                        <span class="stat__value">{ props.blocks_total }</span>
                        <span class="stat__label">{"Total Blocks"}</span>
                    </div>
                    <div class="stat">
                        <span class="stat__value">{ &pct_str }</span>
                        <span class="stat__label">{"Complete"}</span>
                    </div>
                </div>

                // Log area
                <div class="log">
                    <div class="log__header">{"Live Log"}</div>
                    <div class="log__body">
                        { for props.log.iter().map(|line| html! {
                            <div class="log__line">{ line }</div>
                        })}
                        if props.log.is_empty() {
                            <div class="log__line log__line--dim">{"Waiting for first block…"}</div>
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}
