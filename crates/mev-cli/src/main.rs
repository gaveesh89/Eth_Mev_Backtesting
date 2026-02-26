use chrono::NaiveDate;
use clap::{ArgAction, Args, Parser, Subcommand};
use color_eyre::eyre::{eyre, Context, Result};
use comfy_table::presets::UTF8_BORDERS_ONLY;
use comfy_table::Table;
use indicatif::{ProgressBar, ProgressStyle};
use mev_analysis::pnl::{compute_pnl, compute_range_stats, format_eth};
use mev_analysis::scc_detector::detect_arbitrage;
use mev_analysis::transfer_graph::{parse_transfer_log, TxTransferGraph};
use mev_data::blocks::BlockFetcher;
use mev_data::mempool::download_and_store;
use mev_data::store::{IntraBlockArbRow, Store};
use mev_sim::decoder::addresses;
use mev_sim::ordering::{order_by_egp, order_by_profit};
use mev_sim::strategies::arbitrage::{scan_for_arb, DEFAULT_ARB_PAIRS};
use mev_sim::strategies::cex_dex_arb::{micro_usd_to_cex_price_fp, scan_cex_dex, CexPricePoint};
use mev_sim::strategies::dex_dex_intra::scan_default_dex_dex_intra_block;
use mev_sim::EvmFork;
use std::path::{Path, PathBuf};
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone)]
struct AppContext {
    db_path: String,
    rpc_url: Option<String>,
}

#[derive(Parser, Debug)]
#[command(name = "mev-backtest")]
#[command(about = "Educational Ethereum MEV backtesting toolkit")]
#[command(version)]
struct Cli {
    #[arg(long, short = 'v', action = ArgAction::Count, global = true)]
    verbose: u8,

    #[arg(long, short = 'q', global = true)]
    quiet: bool,

    #[arg(long, global = true, default_value = "data/mev.sqlite")]
    db_path: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Fetch(FetchArgs),
    IngestCex(IngestCexArgs),
    Simulate(SimulateArgs),
    Analyze(AnalyzeArgs),
    Status(StatusArgs),
    /// Diagnose swap activity in a block to assess scanner coverage.
    Diagnose(DiagnoseArgs),
    /// Classify MEV in a block using SCC transfer-graph analysis.
    Classify(ClassifyArgs),
}

#[derive(Args, Debug)]
struct FetchArgs {
    #[arg(long)]
    start_block: u64,

    #[arg(long)]
    end_block: u64,

    #[arg(long)]
    date: Option<String>,

    #[arg(long, default_value = "data/mempool")]
    data_dir: PathBuf,
}

#[derive(Args, Debug)]
struct SimulateArgs {
    #[arg(long)]
    block: u64,

    #[arg(long, default_value = "egp")]
    algorithm: String,
}

#[derive(Args, Debug)]
struct IngestCexArgs {
    #[arg(long)]
    csv: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct AnalyzeArgs {
    #[arg(long)]
    start_block: u64,

    #[arg(long)]
    end_block: u64,

    #[arg(long, default_value = "table")]
    output: String,
}

#[derive(Args, Debug)]
struct StatusArgs {
    #[arg(long)]
    block: Option<u64>,
}

/// Arguments for the `diagnose` subcommand.
///
/// Counts V2/V3 Swap events per block, splits scanned vs unscanned pools,
/// reads reserves + cross-DEX spreads, and checks CEX timestamp alignment.
#[derive(Args, Debug)]
struct DiagnoseArgs {
    /// Target block number to diagnose.
    #[arg(long)]
    block: u64,
}

/// Arguments for the `classify` subcommand.
///
/// Runs EigenPhi-style SCC analysis on stored tx receipt logs to detect
/// protocol-agnostic arbitrage. Operates entirely from stored data.
#[derive(Args, Debug)]
struct ClassifyArgs {
    /// Starting block number.
    #[arg(long)]
    start_block: u64,

    /// Ending block number (inclusive).
    #[arg(long)]
    end_block: u64,

    /// Output format: table (default) or json.
    #[arg(long, default_value = "table")]
    output: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet)?;

    let ctx = AppContext {
        db_path: cli.db_path,
        rpc_url: std::env::var("MEV_RPC_URL").ok(),
    };

    match cli.command {
        Commands::Fetch(args) => handle_fetch(&ctx, args).await,
        Commands::IngestCex(args) => handle_ingest_cex(&ctx, args).await,
        Commands::Simulate(args) => handle_simulate(&ctx, args).await,
        Commands::Analyze(args) => handle_analyze(&ctx, args).await,
        Commands::Status(args) => handle_status(&ctx, args).await,
        Commands::Diagnose(args) => handle_diagnose(&ctx, args).await,
        Commands::Classify(args) => handle_classify(&ctx, args).await,
    }
}

fn init_tracing(verbose: u8, quiet: bool) -> Result<()> {
    let level = if quiet {
        Level::WARN
    } else {
        match verbose {
            0 => Level::INFO,
            1 => Level::DEBUG,
            _ => Level::TRACE,
        }
    };

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level.as_str()))
        .wrap_err("failed to initialize tracing filter")?;

    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

async fn handle_fetch(ctx: &AppContext, args: FetchArgs) -> Result<()> {
    if args.start_block > args.end_block {
        return Err(eyre!(
            "invalid range: start-block {} is greater than end-block {}",
            args.start_block,
            args.end_block
        ));
    }

    let rpc_url = ctx
        .rpc_url
        .as_deref()
        .ok_or_else(|| eyre!("MEV_RPC_URL is required for fetch command"))?;

    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;
    let fetcher = BlockFetcher::new(rpc_url).await?;

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .wrap_err("failed to create progress style")?,
    );
    pb.set_message("fetching blocks from RPC");
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    fetcher
        .fetch_range(args.start_block, args.end_block, &store)
        .await
        .wrap_err("failed to fetch block range")?;

    pb.set_message("downloading mempool parquet day");
    let mempool_inserted = if let Some(date_str) = args.date {
        let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
            .wrap_err("invalid --date format, expected YYYY-MM-DD")?;

        ensure_dir(&args.data_dir)?;
        download_and_store(date, &store, &args.data_dir)
            .await
            .wrap_err("failed to download and store mempool parquet")?
    } else {
        0
    };

    pb.finish_with_message("fetch completed");
    info!(
        start_block = args.start_block,
        end_block = args.end_block,
        mempool_inserted,
        db_path = %ctx.db_path,
        "fetch command finished"
    );

    Ok(())
}

async fn handle_ingest_cex(ctx: &AppContext, args: IngestCexArgs) -> Result<()> {
    if args.csv.is_empty() {
        return Err(eyre!("at least one --csv path is required"));
    }

    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;
    let mut rows = Vec::<(&str, u64, i64, i64, i64, i64)>::new();
    let pair = "ETHUSDC";

    for path in &args.csv {
        let content = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;

        for (line_number, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parts: Vec<&str> = trimmed.split(',').collect();
            if parts.len() < 6 {
                tracing::debug!(
                    file = %path.display(),
                    line_number,
                    "skipping malformed kline row"
                );
                continue;
            }

            let open_time_ms = match parts[0].parse::<u64>() {
                Ok(value) => value,
                Err(_) => continue,
            };

            // Convert f64 prices to micro-USD (price × 1e6) at the intake boundary.
            // This is the only place f64 prices are touched; after this everything is integer.
            let open = match parts[1].parse::<f64>() {
                Ok(value) => (value * 1_000_000.0) as i64,
                Err(_) => continue,
            };
            let high = match parts[2].parse::<f64>() {
                Ok(value) => (value * 1_000_000.0) as i64,
                Err(_) => continue,
            };
            let low = match parts[3].parse::<f64>() {
                Ok(value) => (value * 1_000_000.0) as i64,
                Err(_) => continue,
            };
            let close = match parts[4].parse::<f64>() {
                Ok(value) => (value * 1_000_000.0) as i64,
                Err(_) => continue,
            };

            rows.push((pair, open_time_ms / 1000, open, close, high, low));
        }
    }

    let inserted = store
        .insert_cex_prices(&rows)
        .wrap_err("failed to insert cex_prices rows")?;

    info!(rows = rows.len(), inserted, "ingest-cex completed");
    Ok(())
}

async fn handle_simulate(ctx: &AppContext, args: SimulateArgs) -> Result<()> {
    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    // Check block exists
    let block = store
        .get_block(args.block)
        .wrap_err("failed to query block")?
        .ok_or_else(|| eyre!("block {} not found in database", args.block))?;

    // Parse base fee
    let base_fee = parse_hex_u128(&block.base_fee_per_gas, "base_fee_per_gas")?;

    // Run ordering algorithm(s)
    let mut results = Vec::new();
    let algorithms = if args.algorithm.to_lowercase() == "both" {
        vec!["egp", "profit", "arbitrage", "cex_dex", "dex_dex_intra"]
    } else {
        vec![args.algorithm.as_str()]
    };

    let requires_mempool = algorithms
        .iter()
        .any(|algo| *algo == "egp" || *algo == "profit");
    let mempool_txs = if requires_mempool {
        let txs = store
            .get_mempool_txs_for_block(args.block)
            .wrap_err("failed to query mempool transactions")?;
        if txs.is_empty() {
            info!(
                block_number = args.block,
                "no mempool transactions found for mempool-dependent algorithm"
            );
            return Ok(());
        }
        txs
    } else {
        Vec::new()
    };
    const ARB_GAS_ESTIMATE_UNITS: u64 = 200_000;

    for algo in algorithms {
        let (ordered_txs, rejected, tx_count, total_gas, estimated_value) = match algo {
            "egp" => {
                let (ordered_txs, rejected) = order_by_egp(mempool_txs.clone(), base_fee);

                let mut value_evm = EvmFork::at_block(args.block, &block)
                    .wrap_err("failed to create EVM fork for value estimation")?;
                let mut total_gas = 0_u64;
                let mut estimated_value = 0_u128;
                for tx in &ordered_txs {
                    let sim = value_evm
                        .simulate_tx(tx)
                        .wrap_err_with(|| format!("failed to estimate ordered tx {}", tx.hash))?;
                    if sim.success {
                        total_gas = total_gas.saturating_add(sim.gas_used);
                        estimated_value = estimated_value.saturating_add(sim.coinbase_payment);
                    }
                }

                let tx_count = ordered_txs.len();
                (ordered_txs, rejected, tx_count, total_gas, estimated_value)
            }
            "profit" => {
                let mut profit_evm =
                    EvmFork::at_block(args.block, &block).wrap_err("failed to create EVM fork")?;
                let ordered_txs = order_by_profit(mempool_txs.clone(), &mut profit_evm, base_fee)
                    .await
                    .wrap_err_with(|| format!("failed to order by {}", algo))?;

                let mut value_evm = EvmFork::at_block(args.block, &block)
                    .wrap_err("failed to create EVM fork for value estimation")?;
                let mut total_gas = 0_u64;
                let mut estimated_value = 0_u128;
                for tx in &ordered_txs {
                    let sim = value_evm
                        .simulate_tx(tx)
                        .wrap_err_with(|| format!("failed to estimate ordered tx {}", tx.hash))?;
                    if sim.success {
                        total_gas = total_gas.saturating_add(sim.gas_used);
                        estimated_value = estimated_value.saturating_add(sim.coinbase_payment);
                    }
                }

                let tx_count = ordered_txs.len();
                (ordered_txs, 0, tx_count, total_gas, estimated_value)
            }
            "arbitrage" => {
                let rpc_url = ctx
                    .rpc_url
                    .as_deref()
                    .ok_or_else(|| eyre!("MEV_RPC_URL is required for arbitrage strategy"))?;
                let state_block = args.block.saturating_sub(1);
                let opportunities =
                    scan_for_arb(rpc_url, state_block, base_fee, &DEFAULT_ARB_PAIRS)
                        .await
                        .wrap_err("failed to scan arbitrage opportunities")?;

                let weth_opportunity_count = opportunities
                    .iter()
                    .filter(|opp| opp.token_a == addresses::WETH)
                    .count();
                let estimated_value = opportunities
                    .iter()
                    .filter(|opp| opp.token_a == addresses::WETH)
                    .map(|opp| opp.net_profit_wei)
                    .sum::<u128>();
                let total_gas =
                    (weth_opportunity_count as u64).saturating_mul(ARB_GAS_ESTIMATE_UNITS);

                (
                    Vec::new(),
                    0,
                    weth_opportunity_count,
                    total_gas,
                    estimated_value,
                )
            }
            "cex_dex" => {
                let rpc_url = ctx
                    .rpc_url
                    .as_deref()
                    .ok_or_else(|| eyre!("MEV_RPC_URL is required for cex_dex strategy"))?;
                let cex_price_point =
                    match store
                        .get_nearest_cex_close_price_micro("ETHUSDC", block.timestamp)
                        .wrap_err("failed to query cex close price point")?
                    {
                        Some((timestamp_s, close_micro)) => {
                            let close_price_fp =
                                micro_usd_to_cex_price_fp(close_micro);
                            Some(CexPricePoint {
                                timestamp_s,
                                close_price_fp,
                            })
                        }
                        None => None,
                    };

                let state_block = args.block.saturating_sub(1);
                let maybe_opportunity = scan_cex_dex(
                    rpc_url,
                    state_block,
                    block.timestamp,
                    cex_price_point,
                )
                    .await
                    .wrap_err("failed to scan cex_dex opportunity")?;

                let tx_count = usize::from(maybe_opportunity.is_some());
                let estimated_value = maybe_opportunity
                    .as_ref()
                    .map(|opportunity| opportunity.profit_wei)
                    .unwrap_or(0);
                let total_gas = (tx_count as u64).saturating_mul(ARB_GAS_ESTIMATE_UNITS);

                (Vec::new(), 0, tx_count, total_gas, estimated_value)
            }
            "dex_dex_intra" => {
                let rpc_url = ctx
                    .rpc_url
                    .as_deref()
                    .ok_or_else(|| eyre!("MEV_RPC_URL is required for dex_dex_intra strategy"))?;

                let opportunities =
                    scan_default_dex_dex_intra_block(rpc_url, args.block, base_fee)
                        .await
                        .wrap_err("failed to scan intra-block dex_dex opportunities")?;

                let rows: Vec<IntraBlockArbRow> = opportunities
                    .iter()
                    .map(|event| {
                        (
                            event.block_number,
                            event.after_tx_index,
                            event.after_log_index,
                            format!("{:#x}", event.pool_a),
                            format!("{:#x}", event.pool_b),
                            event.spread_bps as i64,
                            format!("0x{:x}", event.profit_wei),
                            event.direction.clone(),
                            event.verdict.clone(),
                        )
                    })
                    .collect();
                let _inserted = store
                    .insert_intra_block_arbs(&rows)
                    .wrap_err("failed to insert intra_block_arbs rows")?;

                let tx_count = opportunities.len();
                let estimated_value = opportunities
                    .iter()
                    .map(|event| event.profit_wei)
                    .sum::<u128>();
                let total_gas = (tx_count as u64).saturating_mul(ARB_GAS_ESTIMATE_UNITS);

                (Vec::new(), 0, tx_count, total_gas, estimated_value)
            }
            _ => {
                return Err(eyre!(
                    "unknown algorithm '{}'; use 'egp', 'profit', 'arbitrage', 'cex_dex', 'dex_dex_intra', or 'both'",
                    algo
                ))
            }
        };

        // Persistence to SQLite
        let algo_name = match algo {
            "egp" => "egp",
            "profit" => "profit",
            "arbitrage" => "arbitrage",
            "cex_dex" => "cex_dex",
            "dex_dex_intra" => "dex_dex_intra",
            _ => unreachable!(),
        };
        store
            .insert_simulation_result(
                args.block,
                algo_name,
                tx_count,
                total_gas,
                &format!("0x{:x}", estimated_value),
                "0x0",
            )
            .wrap_err("failed to insert simulation result")?;

        results.push((algo_name, ordered_txs, rejected, tx_count));
    }

    // Print tx-by-tx table for single block
    for (algo_name, txs, rejected_count, selected_count) in results {
        println!(
            "\n=== {} Algorithm: {} ===",
            algo_name.to_uppercase(),
            args.block
        );
        println!(
            "Ordered: {}, Rejected: {}\n",
            selected_count, rejected_count
        );

        let mut table = Table::new();
        table.load_preset(UTF8_BORDERS_ONLY);
        table.set_header(vec![
            "Tx Hash (truncated)",
            "Gas Limit",
            "Gas Price (gwei)",
            "Type",
        ]);

        for tx in txs.iter().take(20) {
            let hash_short = if tx.hash.len() > 10 {
                format!("{}...{}", &tx.hash[2..8], &tx.hash[tx.hash.len() - 4..])
            } else {
                tx.hash.clone()
            };

            let gas_price_str = if tx.tx_type == 2 {
                &tx.max_fee_per_gas
            } else {
                &tx.gas_price
            };
            let gas_price_wei = parse_hex_u128(gas_price_str, "gas_price")?;
            let gas_price_gwei = gas_price_wei / 1_000_000_000;
            let type_str = if tx.tx_type == 2 {
                "EIP-1559"
            } else {
                "Legacy"
            };

            table.add_row(vec![
                hash_short,
                tx.gas_limit.to_string(),
                gas_price_gwei.to_string(),
                type_str.to_string(),
            ]);
        }

        println!("{}\n", table);
    }

    info!(
        block_number = args.block,
        algorithm = %args.algorithm,
        "simulate command completed"
    );

    Ok(())
}

async fn handle_analyze(ctx: &AppContext, args: AnalyzeArgs) -> Result<()> {
    if args.start_block > args.end_block {
        return Err(eyre!(
            "invalid range: start-block {} is greater than end-block {}",
            args.start_block,
            args.end_block
        ));
    }

    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    // Collect PnL for all blocks in range
    let mut pnl_records = Vec::new();
    for block_num in args.start_block..=args.end_block {
        match compute_pnl(block_num, &store) {
            Ok(pnl) => pnl_records.push(pnl),
            Err(e) => {
                tracing::debug!(block = block_num, error = %e, "skipped block (no data)");
            }
        }
    }

    if pnl_records.is_empty() {
        return Err(eyre!(
            "no blocks found in range {}-{}",
            args.start_block,
            args.end_block
        ));
    }

    let stats = compute_range_stats(&pnl_records);

    match args.output.to_lowercase().as_str() {
        "table" => print_pnl_table(&pnl_records, &stats)?,
        "json" => print_pnl_json(&pnl_records, &stats)?,
        "csv" => print_pnl_csv(&pnl_records, &stats)?,
        _ => {
            return Err(eyre!(
                "unknown output format '{}'; use 'table', 'json', or 'csv'",
                args.output
            ))
        }
    }

    info!(
        start_block = args.start_block,
        end_block = args.end_block,
        blocks_analyzed = pnl_records.len(),
        output = %args.output,
        "analyze command completed"
    );

    Ok(())
}

fn color_capture_rate(rate: f64) -> &'static str {
    if rate > 0.8 {
        "\x1b[32m" // Green
    } else if rate >= 0.5 {
        "\x1b[33m" // Yellow
    } else {
        "\x1b[31m" // Red
    }
}

const COLOR_RESET: &str = "\x1b[0m";

fn print_pnl_table(
    pnl_records: &[mev_analysis::pnl::BlockPnL],
    stats: &mev_analysis::pnl::RangeStats,
) -> Result<()> {
    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(vec![
        "Block",
        "Actual Value",
        "Simulated Value",
        "MEV Captured",
        "Capture Rate",
    ]);

    for pnl in pnl_records {
        let capture_rate_str = format!("{:.2}%", pnl.capture_rate * 100.0);
        let color = color_capture_rate(pnl.capture_rate);
        let colored_rate = format!("{}{}{}", color, capture_rate_str, COLOR_RESET);

        table.add_row(vec![
            pnl.block_number.to_string(),
            format_eth(pnl.actual_block_value_wei),
            format_eth(pnl.simulated_block_value_wei),
            format_eth(pnl.mev_captured_wei),
            colored_rate,
        ]);
    }

    println!("{}\n", table);

    // Print summary
    println!("Summary (blocks: {}):", stats.block_count);
    println!(
        "  Actual Total Value:    {}",
        format_eth(stats.total_actual_block_value_wei)
    );
    println!(
        "  Simulated Total Value: {}",
        format_eth(stats.total_simulated_block_value_wei)
    );
    println!(
        "  MEV Captured:          {}",
        format_eth(stats.total_mev_captured_wei)
    );
    println!(
        "  Mean Capture Rate:     {:.2}%\n",
        stats.mean_capture_rate * 100.0
    );

    Ok(())
}

fn print_pnl_json(
    pnl_records: &[mev_analysis::pnl::BlockPnL],
    stats: &mev_analysis::pnl::RangeStats,
) -> Result<()> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct JsonOutput {
        blocks: Vec<JsonBlock>,
        summary: JsonSummary,
    }

    #[derive(Serialize)]
    struct JsonBlock {
        block_number: u64,
        actual_value_wei: u128,
        simulated_value_wei: u128,
        mev_captured_wei: u128,
        capture_rate: f64,
    }

    #[derive(Serialize)]
    struct JsonSummary {
        block_count: usize,
        total_actual_value_wei: u128,
        total_simulated_value_wei: u128,
        total_mev_captured_wei: u128,
        mean_capture_rate: f64,
    }

    let blocks: Vec<JsonBlock> = pnl_records
        .iter()
        .map(|pnl| JsonBlock {
            block_number: pnl.block_number,
            actual_value_wei: pnl.actual_block_value_wei,
            simulated_value_wei: pnl.simulated_block_value_wei,
            mev_captured_wei: pnl.mev_captured_wei,
            capture_rate: pnl.capture_rate,
        })
        .collect();

    let summary = JsonSummary {
        block_count: stats.block_count,
        total_actual_value_wei: stats.total_actual_block_value_wei,
        total_simulated_value_wei: stats.total_simulated_block_value_wei,
        total_mev_captured_wei: stats.total_mev_captured_wei,
        mean_capture_rate: stats.mean_capture_rate,
    };

    let output = JsonOutput { blocks, summary };
    let json_str = serde_json::to_string_pretty(&output).wrap_err("failed to serialize JSON")?;
    println!("{}", json_str);

    Ok(())
}

fn print_pnl_csv(
    pnl_records: &[mev_analysis::pnl::BlockPnL],
    _stats: &mev_analysis::pnl::RangeStats,
) -> Result<()> {
    println!("block_number,actual_value_wei,simulated_value_wei,mev_captured_wei,capture_rate");

    for pnl in pnl_records {
        println!(
            "{},{},{},{},{}",
            pnl.block_number,
            pnl.actual_block_value_wei,
            pnl.simulated_block_value_wei,
            pnl.mev_captured_wei,
            pnl.capture_rate,
        );
    }

    Ok(())
}

async fn handle_status(ctx: &AppContext, _args: StatusArgs) -> Result<()> {
    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    // Get block range
    let (min_block, max_block, block_count) = store
        .get_block_range()
        .wrap_err("failed to query block range")?;

    // Get mempool timestamp range
    let (min_ts_ms, max_ts_ms, mempool_count) = store
        .get_mempool_timestamp_range()
        .wrap_err("failed to query mempool timestamp range")?;

    // Get simulation count
    let sim_count = store
        .count_simulations()
        .wrap_err("failed to query simulation count")?;

    // Get database file size (handle in-memory databases gracefully)
    let db_size_str = if ctx.db_path == ":memory:" {
        "N/A (in-memory)".to_string()
    } else {
        match std::fs::metadata(&ctx.db_path) {
            Ok(metadata) => format!("{} MB", metadata.len() / 1_000_000),
            Err(_) => "N/A (file not found)".to_string(),
        }
    };

    // Format timestamps to datetime
    let min_datetime = if min_ts_ms > 0 {
        chrono::DateTime::from_timestamp_millis(min_ts_ms as i64)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "N/A".to_string())
    } else {
        "N/A".to_string()
    };

    let max_datetime = if max_ts_ms > 0 {
        chrono::DateTime::from_timestamp_millis(max_ts_ms as i64)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "N/A".to_string())
    } else {
        "N/A".to_string()
    };

    // Build table
    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(vec!["Metric", "Value"]);

    table.add_row(vec!["Database Path", ctx.db_path.as_str()]);
    table.add_row(vec!["DB Size", &db_size_str]);

    if block_count > 0 {
        table.add_row(vec!["Blocks", &format!("{}", block_count)]);
        table.add_row(vec![
            "Block Range",
            &format!("{} - {}", min_block, max_block),
        ]);
    } else {
        table.add_row(vec!["Blocks", "0"]);
        table.add_row(vec!["Block Range", "No blocks in database"]);
    }

    if mempool_count > 0 {
        table.add_row(vec!["Mempool Transactions", &format!("{}", mempool_count)]);
        table.add_row(vec!["Mempool Date Range (min)", &min_datetime]);
        table.add_row(vec!["Mempool Date Range (max)", &max_datetime]);
    } else {
        table.add_row(vec!["Mempool Transactions", "0"]);
        table.add_row(vec!["Mempool Date Range", "No transactions in database"]);
    }

    table.add_row(vec!["Simulations", &format!("{}", sim_count)]);

    println!("\n{}\n", table);

    info!(
        blocks = block_count,
        mempool_txs = mempool_count,
        simulations = sim_count,
        db_path = %ctx.db_path,
        "status command completed"
    );

    Ok(())
}

/// Diagnose swap activity in a single block.
///
/// Counts V2/V3 Swap events, splits by scanned vs unscanned pools,
/// reads UniV2 reserves and cross-DEX spreads, and checks CEX timestamp
/// alignment. This helps assess whether the scanner's pool universe explains
/// a zero-opportunity result.
///
/// # Errors
/// Returns error if RPC calls fail or DB cannot be opened.
async fn handle_diagnose(ctx: &AppContext, args: DiagnoseArgs) -> Result<()> {
    let rpc_url = ctx
        .rpc_url
        .as_deref()
        .ok_or_else(|| eyre!("MEV_RPC_URL is required for diagnose command"))?;

    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    let client = reqwest::Client::new();
    let block_number = args.block;

    // --- V2 Swap event topic ---
    let v2_swap_topic = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";
    let v3_swap_topic = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";

    // Resolve scanned pool addresses from DEFAULT_ARB_PAIRS
    let reserve_reader =
        mev_sim::strategies::arbitrage::ReserveReader::new(rpc_url, block_number.saturating_sub(1));

    let mut scanned_addrs: Vec<String> = Vec::new();
    for pair in &DEFAULT_ARB_PAIRS {
        if let Some(addr) = reserve_reader
            .get_pair(pair.dex_a_factory, pair.token_a, pair.token_b)
            .await?
        {
            scanned_addrs.push(format!("{addr:#x}"));
        }
        if let Some(addr) = reserve_reader
            .get_pair(pair.dex_b_factory, pair.token_a, pair.token_b)
            .await?
        {
            scanned_addrs.push(format!("{addr:#x}"));
        }
    }

    let block_hex = format!("0x{block_number:x}");

    // --- Count all V2 Swap events ---
    let all_v2: Vec<serde_json::Value> = {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "eth_getLogs",
            "params": [{"fromBlock": &block_hex, "toBlock": &block_hex, "topics": [v2_swap_topic]}]
        });
        let resp: serde_json::Value = client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .wrap_err("eth_getLogs V2 failed")?
            .json()
            .await?;
        resp.get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
    };

    // --- Count all V3 Swap events ---
    let all_v3: Vec<serde_json::Value> = {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "eth_getLogs",
            "params": [{"fromBlock": &block_hex, "toBlock": &block_hex, "topics": [v3_swap_topic]}]
        });
        let resp: serde_json::Value = client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .wrap_err("eth_getLogs V3 failed")?
            .json()
            .await?;
        resp.get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
    };

    // Split V2 swaps by scanned/unscanned
    let scanned_lower: Vec<String> = scanned_addrs.iter().map(|a| a.to_lowercase()).collect();
    let v2_on_scanned = all_v2
        .iter()
        .filter(|log| {
            log.get("address")
                .and_then(|a| a.as_str())
                .map(|a| scanned_lower.contains(&a.to_lowercase()))
                .unwrap_or(false)
        })
        .count();
    let v2_on_unscanned = all_v2.len() - v2_on_scanned;

    // Count unique unscanned pools
    let mut unscanned_pools: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for log in &all_v2 {
        if let Some(addr) = log.get("address").and_then(|a| a.as_str()) {
            let addr_lower = addr.to_lowercase();
            if !scanned_lower.contains(&addr_lower) {
                *unscanned_pools.entry(addr_lower).or_default() += 1;
            }
        }
    }

    // --- Read reserves and compute spreads ---
    let mut spread_rows: Vec<(String, f64, f64, f64)> = Vec::new();
    for pair in &DEFAULT_ARB_PAIRS {
        let pair_name = if pair.token_b == addresses::USDC {
            "WETH/USDC"
        } else if pair.token_b == addresses::USDT {
            "WETH/USDT"
        } else {
            "WETH/DAI"
        };

        let a_addr = reserve_reader
            .get_pair(pair.dex_a_factory, pair.token_a, pair.token_b)
            .await?;
        let b_addr = reserve_reader
            .get_pair(pair.dex_b_factory, pair.token_a, pair.token_b)
            .await?;

        if let (Some(a), Some(b)) = (a_addr, b_addr) {
            let (a_r0, a_r1, _) = reserve_reader.get_reserves(a).await.unwrap_or((0, 0, 0));
            let (b_r0, b_r1, _) = reserve_reader.get_reserves(b).await.unwrap_or((0, 0, 0));

            if a_r0 > 0 && a_r1 > 0 && b_r0 > 0 && b_r1 > 0 {
                // price = r0/r1 (quote/base), but token ordering varies
                let a_price = a_r0 as f64 / a_r1 as f64;
                let b_price = b_r0 as f64 / b_r1 as f64;
                let spread = (a_price - b_price).abs() / a_price.min(b_price) * 10000.0;
                spread_rows.push((pair_name.to_string(), a_price, b_price, spread));
            }
        }
    }

    // --- CEX timestamp check ---
    let block_ts = store
        .get_block(block_number)
        .ok()
        .flatten()
        .map(|b| b.timestamp);
    let cex_delta = if let Some(ts) = block_ts {
        store
            .get_nearest_cex_close_price_micro("ETHUSDC", ts)
            .ok()
            .flatten()
            .map(|(cex_ts, _price)| cex_ts.abs_diff(ts))
    } else {
        None
    };

    // --- Print results ---
    let mut table = Table::new();
    table.load_preset(UTF8_BORDERS_ONLY);
    table.set_header(vec!["Metric", "Value"]);

    table.add_row(vec!["Block", &format!("{block_number}")]);
    table.add_row(vec!["Scanned pools", &format!("{}", scanned_addrs.len())]);
    table.add_row(vec!["V2 Swap events (total)", &format!("{}", all_v2.len())]);
    table.add_row(vec!["  on scanned pools", &format!("{v2_on_scanned}")]);
    table.add_row(vec!["  on unscanned pools", &format!("{v2_on_unscanned}")]);
    table.add_row(vec!["V3 Swap events (total)", &format!("{}", all_v3.len())]);
    table.add_row(vec![
        "Unscanned V2 pool count",
        &format!("{}", unscanned_pools.len()),
    ]);

    // Coverage percentage
    let total_swaps = all_v2.len() + all_v3.len();
    let coverage_pct = if total_swaps > 0 {
        v2_on_scanned as f64 / total_swaps as f64 * 100.0
    } else {
        0.0
    };
    table.add_row(vec![
        "Scanner coverage",
        &format!("{coverage_pct:.1}% of swap events"),
    ]);

    println!("\n{table}\n");

    // Cross-DEX spreads
    if !spread_rows.is_empty() {
        let mut spread_table = Table::new();
        spread_table.load_preset(UTF8_BORDERS_ONLY);
        spread_table.set_header(vec![
            "Pair",
            "UniV2 Price",
            "Sushi Price",
            "Spread (bps)",
            "vs 60 bps floor",
        ]);
        for (name, a_p, b_p, spread) in &spread_rows {
            let verdict = if *spread > 60.0 {
                "ABOVE — should detect"
            } else if *spread > 10.0 {
                "10-60 bps — not profitable"
            } else {
                "Below prefilter"
            };
            spread_table.add_row(vec![
                name.as_str(),
                &format!("{a_p:.8}"),
                &format!("{b_p:.8}"),
                &format!("{spread:.2}"),
                verdict,
            ]);
        }
        println!("{spread_table}\n");
    }

    // CEX alignment
    if let Some(delta) = cex_delta {
        let status = if delta <= 3 { "OK" } else { "STALE (>3s)" };
        info!(cex_delta_s = delta, status, "CEX timestamp alignment");
    } else {
        tracing::warn!("No CEX data or block not in DB for timestamp alignment check");
    }

    // Top unscanned pools
    if !unscanned_pools.is_empty() {
        let mut sorted: Vec<_> = unscanned_pools.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        info!("Top unscanned V2 pools:");
        for (addr, cnt) in sorted.iter().take(5) {
            info!(pool = %addr, swaps = cnt, "unscanned pool");
        }
    }

    info!(
        block = block_number,
        v2_total = all_v2.len(),
        v2_scanned = v2_on_scanned,
        v3_total = all_v3.len(),
        coverage_pct = format!("{coverage_pct:.1}%"),
        "diagnose command completed"
    );

    Ok(())
}

/// Classify MEV in a block range using SCC transfer-graph analysis.
///
/// For each block, reads stored Transfer event logs from the `tx_logs` table,
/// builds a [`TxTransferGraph`] per transaction, and runs Tarjan's SCC algorithm
/// to detect protocol-agnostic arbitrage. Results are printed as a table or JSON.
///
/// This implements the EigenPhi 6-step algorithm:
/// 1. Parse Transfer events from stored logs
/// 2. Build directed transfer graph per tx
/// 3. Find strongly connected components
/// 4. Locate profiteer (closest node to tx.from/tx.to)
/// 5. Calculate net balance from SCC-internal edges
/// 6. Positive net → arbitrage
///
/// # Errors
/// Returns error if the database cannot be opened or block data is missing.
async fn handle_classify(ctx: &AppContext, args: ClassifyArgs) -> Result<()> {
    use alloy::primitives::Address;

    if args.start_block > args.end_block {
        return Err(eyre!(
            "invalid range: start-block {} is greater than end-block {}",
            args.start_block,
            args.end_block
        ));
    }

    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    let block_count = args.end_block - args.start_block + 1;
    let pb = ProgressBar::new(block_count);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} blocks")
            .wrap_err("failed to create progress style")?
            .progress_chars("#>-"),
    );

    /// One detected arbitrage for output.
    #[derive(Debug, serde::Serialize)]
    struct ArbResult {
        block_number: u64,
        tx_hash: String,
        profiteer: String,
        scc_nodes: usize,
        scc_edges: usize,
        tokens_involved: usize,
        profit_summary: String,
    }

    let mut all_results: Vec<ArbResult> = Vec::new();
    let mut blocks_scanned: u64 = 0;
    let mut txs_scanned: u64 = 0;

    for block_num in args.start_block..=args.end_block {
        // Get block transactions (needed for tx.from / tx.to)
        let block_txs = store.get_block_txs(block_num).wrap_err_with(|| {
            format!("failed to query block_transactions for block {block_num}")
        })?;

        if block_txs.is_empty() {
            pb.inc(1);
            continue;
        }

        // Get Transfer logs for this block
        let transfer_logs = store
            .get_transfer_logs_for_block(block_num)
            .wrap_err_with(|| format!("failed to query transfer logs for block {block_num}"))?;

        // Build a lookup: tx_hash → (from_address, to_address)
        let tx_meta: std::collections::HashMap<String, (&str, &str)> = block_txs
            .iter()
            .map(|tx| {
                (
                    tx.tx_hash.clone(),
                    (tx.from_address.as_str(), tx.to_address.as_str()),
                )
            })
            .collect();

        // Group transfer logs by tx_hash
        let mut logs_by_tx: std::collections::HashMap<&str, Vec<&mev_data::TxLog>> =
            std::collections::HashMap::new();
        for log in &transfer_logs {
            logs_by_tx
                .entry(log.tx_hash.as_str())
                .or_default()
                .push(log);
        }

        // Process each transaction with transfer logs
        for (tx_hash, logs) in &logs_by_tx {
            txs_scanned += 1;

            // Parse logs into TokenTransfers
            let transfers: Vec<_> = logs
                .iter()
                .filter_map(|log| parse_transfer_log(log))
                .collect();

            if transfers.len() < 2 {
                continue; // Need at least 2 transfers for a cycle
            }

            // Get tx.from and tx.to for this transaction
            let (tx_from_str, tx_to_str) = match tx_meta.get(*tx_hash) {
                Some(meta) => *meta,
                None => continue,
            };

            let tx_from = tx_from_str.parse::<Address>().unwrap_or(Address::ZERO);
            let tx_to = tx_to_str.parse::<Address>().unwrap_or(Address::ZERO);

            // Build transfer graph
            let graph = TxTransferGraph::from_transfers(&transfers, tx_from, tx_to);

            // Run SCC detection
            if let Some(detection) = detect_arbitrage(&graph) {
                if detection.is_arbitrage {
                    // Format profit summary
                    let profit_summary: Vec<String> = detection
                        .profit_tokens
                        .iter()
                        .map(|(token, amount)| format!("{token:#x}: {amount}"))
                        .collect();

                    all_results.push(ArbResult {
                        block_number: block_num,
                        tx_hash: tx_hash.to_string(),
                        profiteer: format!("{:#x}", detection.profiteer),
                        scc_nodes: detection.scc_node_count,
                        scc_edges: detection.scc_edge_count,
                        tokens_involved: detection.tokens_involved,
                        profit_summary: profit_summary.join("; "),
                    });
                }
            }
        }

        blocks_scanned += 1;
        pb.inc(1);
    }

    pb.finish_and_clear();

    // Output results
    match args.output.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&all_results)
                .wrap_err("failed to serialize results to JSON")?;
            println!("{json}");
        }
        _ => {
            // Table output
            let mut table = Table::new();
            table.load_preset(UTF8_BORDERS_ONLY);
            table.set_header(vec![
                "Block",
                "Tx Hash",
                "Profiteer",
                "SCC Nodes",
                "SCC Edges",
                "Tokens",
                "Profit (token: raw amount)",
            ]);

            for result in &all_results {
                table.add_row(vec![
                    &format!("{}", result.block_number),
                    &truncate_hash(&result.tx_hash),
                    &truncate_hash(&result.profiteer),
                    &format!("{}", result.scc_nodes),
                    &format!("{}", result.scc_edges),
                    &format!("{}", result.tokens_involved),
                    &result.profit_summary,
                ]);
            }

            println!("\n{table}\n");

            // Summary
            let mut summary = Table::new();
            summary.load_preset(UTF8_BORDERS_ONLY);
            summary.set_header(vec!["Metric", "Value"]);
            summary.add_row(vec!["Blocks scanned", &format!("{blocks_scanned}")]);
            summary.add_row(vec!["Transactions analyzed", &format!("{txs_scanned}")]);
            summary.add_row(vec![
                "Arbitrage txs detected",
                &format!("{}", all_results.len()),
            ]);

            if !all_results.is_empty() {
                let unique_profiteers: std::collections::HashSet<&str> =
                    all_results.iter().map(|r| r.profiteer.as_str()).collect();
                summary.add_row(vec![
                    "Unique profiteers",
                    &format!("{}", unique_profiteers.len()),
                ]);
            }

            println!("{summary}\n");
            println!("{}", mev_analysis::classify::ACCURACY_DISCLAIMER);
        }
    }

    info!(
        blocks_scanned,
        txs_scanned,
        arbs_detected = all_results.len(),
        "classify command completed"
    );

    Ok(())
}

/// Truncate a hex hash/address for compact table display.
fn truncate_hash(hash: &str) -> String {
    if hash.len() > 14 {
        format!("{}…{}", &hash[..8], &hash[hash.len() - 4..])
    } else {
        hash.to_string()
    }
}

fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .wrap_err_with(|| format!("failed to create data directory {}", path.display()))?;
    Ok(())
}

fn parse_hex_u128(value: &str, context: &str) -> Result<u128> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    if let Some(hex) = trimmed.strip_prefix("0x") {
        if hex.is_empty() {
            return Ok(0);
        }
        return u128::from_str_radix(hex, 16)
            .wrap_err_with(|| format!("failed to parse hex value for {}: {}", context, value));
    }

    trimmed
        .parse::<u128>()
        .wrap_err_with(|| format!("failed to parse decimal value for {}: {}", context, value))
}
