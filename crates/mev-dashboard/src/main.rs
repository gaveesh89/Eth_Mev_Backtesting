//! MEV Backtest Dashboard — browser-based WASM application.
//!
//! Entry point for the Yew single-page app. Manages the three-phase
//! state machine: Setup → Running → Results.

mod analysis;
mod components;
mod rpc;
mod types;

use components::header::Header;
use components::results::Results;
use components::running::Running;
use components::setup::Setup;
use types::{AnalysisResults, AppConfig, RpcBlock, RpcReceipt};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

// ---------------------------------------------------------------------------
// App state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum AppPhase {
    Setup,
    Testing,       // testing RPC connection
    Running,
    Done,
    Error(String),
}

// ---------------------------------------------------------------------------
// App component
// ---------------------------------------------------------------------------

#[function_component(App)]
fn app() -> Html {
    let phase = use_state(|| AppPhase::Setup);
    let progress = use_state(|| 0.0_f64);
    let log_lines = use_state(Vec::<AttrValue>::new);
    let blocks_done = use_state(|| 0_u64);
    let blocks_total = use_state(|| 0_u64);
    let results = use_state(AnalysisResults::default);
    let error_msg = use_state(|| Option::<AttrValue>::None);

    // -----------------------------------------------------------------------
    // "Start Analysis" handler
    // -----------------------------------------------------------------------
    let on_start = {
        let phase = phase.clone();
        let progress = progress.clone();
        let log_lines = log_lines.clone();
        let blocks_done = blocks_done.clone();
        let blocks_total = blocks_total.clone();
        let results = results.clone();
        let error_msg = error_msg.clone();

        Callback::from(move |config: AppConfig| {
            let phase = phase.clone();
            let progress = progress.clone();
            let log_lines = log_lines.clone();
            let blocks_done = blocks_done.clone();
            let blocks_total = blocks_total.clone();
            let results = results.clone();
            let error_msg = error_msg.clone();

            // Validate
            if config.rpc_url.is_empty() {
                error_msg.set(Some("Please enter an RPC URL or API key.".into()));
                return;
            }
            if config.end_block < config.start_block {
                error_msg.set(Some("End block must be ≥ start block.".into()));
                return;
            }
            let total = config.end_block - config.start_block + 1;
            if total > 500 {
                error_msg.set(Some("Max 500 blocks per run to stay within rate limits.".into()));
                return;
            }

            error_msg.set(None);
            phase.set(AppPhase::Testing);
            blocks_total.set(total);
            blocks_done.set(0);
            progress.set(0.0);
            log_lines.set(vec![]);

            spawn_local(async move {
                // --- Test connection ---
                log_lines.set(vec!["Testing RPC connection…".into()]);

                match rpc::test_connection(&config.rpc_url).await {
                    Ok(latest) => {
                        let mut msgs = (*log_lines).clone();
                        msgs.push(format!("Connected — latest block: {latest}").into());
                        log_lines.set(msgs);
                    }
                    Err(e) => {
                        error_msg.set(Some(format!("Connection failed: {e}").into()));
                        phase.set(AppPhase::Setup);
                        return;
                    }
                }

                phase.set(AppPhase::Running);

                // --- Fetch blocks + receipts ---
                let mut block_data: Vec<(RpcBlock, Vec<RpcReceipt>)> = Vec::new();
                let total = config.end_block - config.start_block + 1;

                for num in config.start_block..=config.end_block {
                    let idx = num - config.start_block;

                    // Update progress
                    let pct = (idx + 1) as f64 / total as f64;
                    progress.set(pct);
                    blocks_done.set(idx + 1);

                    let mut msgs = (*log_lines).clone();
                    msgs.push(format!("Fetching block {num}…").into());
                    // Keep last 50 lines
                    if msgs.len() > 50 {
                        msgs.drain(0..msgs.len() - 50);
                    }
                    log_lines.set(msgs);

                    // Yield to let the UI update
                    gloo_timers::future::TimeoutFuture::new(0).await;

                    // Fetch block
                    let block = match rpc::fetch_block(&config.rpc_url, num).await {
                        Ok(b) => b,
                        Err(e) => {
                            let mut msgs = (*log_lines).clone();
                            msgs.push(format!("⚠ Block {num} failed: {e}").into());
                            log_lines.set(msgs);
                            continue;
                        }
                    };

                    // Fetch receipts
                    let receipts = match rpc::fetch_block_receipts(&config.rpc_url, &block).await {
                        Ok(r) => r,
                        Err(e) => {
                            let mut msgs = (*log_lines).clone();
                            msgs.push(format!("⚠ Receipts for block {num} failed: {e}").into());
                            log_lines.set(msgs);
                            Vec::new()
                        }
                    };

                    let tx_count = block.transactions.len();
                    let receipt_count = receipts.len();
                    let mut msgs = (*log_lines).clone();
                    msgs.push(
                        format!("✓ Block {num}: {tx_count} txs, {receipt_count} receipts").into(),
                    );
                    if msgs.len() > 50 {
                        msgs.drain(0..msgs.len() - 50);
                    }
                    log_lines.set(msgs);

                    block_data.push((block, receipts));
                }

                // --- Run analysis ---
                {
                    let mut msgs = (*log_lines).clone();
                    msgs.push("Running MEV analysis…".into());
                    log_lines.set(msgs);
                }

                gloo_timers::future::TimeoutFuture::new(0).await;

                let analysis = analysis::analyze_blocks(&block_data, &config.strategy);

                {
                    let mut msgs = (*log_lines).clone();
                    msgs.push(
                        format!(
                            "✓ Analysis complete: {} opportunities found",
                            analysis.opportunities.len()
                        )
                        .into(),
                    );
                    log_lines.set(msgs);
                }

                results.set(analysis);
                phase.set(AppPhase::Done);
            });
        })
    };

    // -----------------------------------------------------------------------
    // "New Analysis" handler
    // -----------------------------------------------------------------------
    let on_reset = {
        let phase = phase.clone();
        let progress = progress.clone();
        let log_lines = log_lines.clone();
        let blocks_done = blocks_done.clone();
        let error_msg = error_msg.clone();
        Callback::from(move |_: ()| {
            phase.set(AppPhase::Setup);
            progress.set(0.0);
            log_lines.set(vec![]);
            blocks_done.set(0);
            error_msg.set(None);
        })
    };

    // -----------------------------------------------------------------------
    // Render
    // -----------------------------------------------------------------------
    let step_label = match &*phase {
        AppPhase::Setup | AppPhase::Testing => "Configure",
        AppPhase::Running => "Analyzing",
        AppPhase::Done => "Results",
        AppPhase::Error(_) => "Error",
    };

    html! {
        <div class="app">
            <Header step={AttrValue::from(step_label)} />

            <main class="main">
                { match &*phase {
                    AppPhase::Setup | AppPhase::Testing => html! {
                        <Setup
                            on_start={on_start.clone()}
                            error={(*error_msg).clone()}
                            testing={matches!(&*phase, AppPhase::Testing)}
                        />
                    },
                    AppPhase::Running => html! {
                        <Running
                            progress={*progress}
                            log={(*log_lines).clone()}
                            blocks_done={*blocks_done}
                            blocks_total={*blocks_total}
                        />
                    },
                    AppPhase::Done => html! {
                        <Results
                            results={(*results).clone()}
                            on_reset={on_reset.clone()}
                        />
                    },
                    AppPhase::Error(msg) => html! {
                        <div class="card">
                            <div class="alert alert--error">
                                <span class="alert__icon">{"⚠"}</span>
                                { msg }
                            </div>
                            <button
                                class="btn btn--secondary"
                                onclick={
                                    let phase = phase.clone();
                                    Callback::from(move |_: MouseEvent| phase.set(AppPhase::Setup))
                                }
                            >
                                {"← Back to Setup"}
                            </button>
                        </div>
                    },
                }}
            </main>

            <footer class="footer">
                <p>
                    {"MEV Backtest Toolkit — Educational analysis only. "}
                    <a href="https://github.com/gaveesh89/Eth_Mev_Backtesting" target="_blank" rel="noopener">
                        {"View Source"}
                    </a>
                </p>
            </footer>
        </div>
    }
}

// ---------------------------------------------------------------------------
// WASM entry point
// ---------------------------------------------------------------------------

fn main() {
    yew::Renderer::<App>::new().render();
}
