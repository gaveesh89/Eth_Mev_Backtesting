use chrono::NaiveDate;
use clap::{ArgAction, Args, Parser, Subcommand};
use color_eyre::eyre::{eyre, Context, Result};
use comfy_table::presets::UTF8_BORDERS_ONLY;
use comfy_table::Table;
use indicatif::{ProgressBar, ProgressStyle};
use mev_analysis::pnl::{compute_pnl, compute_range_stats, format_eth};
use mev_data::blocks::BlockFetcher;
use mev_data::mempool::download_and_store;
use mev_data::store::Store;
use mev_sim::ordering::{order_by_egp, order_by_profit};
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
    Simulate(SimulateArgs),
    Analyze(AnalyzeArgs),
    Status(StatusArgs),
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
        Commands::Simulate(args) => handle_simulate(&ctx, args).await,
        Commands::Analyze(args) => handle_analyze(&ctx, args).await,
        Commands::Status(args) => handle_status(&ctx, args).await,
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

async fn handle_simulate(ctx: &AppContext, args: SimulateArgs) -> Result<()> {
    let store = Store::new(&ctx.db_path).wrap_err("failed to open SQLite store")?;

    // Check block exists
    let block = store
        .get_block(args.block)
        .wrap_err("failed to query block")?
        .ok_or_else(|| eyre!("block {} not found in database", args.block))?;

    let block_txs = store
        .get_block_txs(args.block)
        .wrap_err("failed to query block transactions")?;

    if block_txs.is_empty() {
        info!(block_number = args.block, "no transactions in block");
        return Ok(());
    }

    // Create EVM fork for simulation
    let mut evm = EvmFork::at_block(args.block, &block).wrap_err("failed to create EVM fork")?;

    // Parse base fee
    let base_fee_hex = &block.base_fee_per_gas;
    let base_fee = if base_fee_hex.starts_with("0x") {
        u128::from_str_radix(base_fee_hex.trim_start_matches("0x"), 16).unwrap_or(0)
    } else {
        base_fee_hex.parse::<u128>().unwrap_or(0)
    };

    // Get mempool transactions for this block
    let mempool_txs = store
        .get_mempool_txs_for_block(args.block)
        .wrap_err("failed to query mempool transactions")?;

    if mempool_txs.is_empty() {
        info!(block_number = args.block, "no mempool transactions found");
        return Ok(());
    }

    // Run ordering algorithm(s)
    let mut results = Vec::new();
    let algorithms = if args.algorithm.to_lowercase() == "both" {
        vec!["egp", "profit"]
    } else {
        vec![args.algorithm.as_str()]
    };

    for algo in algorithms {
        let (ordered_txs, rejected) = if algo == "egp" {
            order_by_egp(mempool_txs.clone(), base_fee)
        } else {
            let ordered = order_by_profit(mempool_txs.clone(), &mut evm, base_fee)
                .await
                .wrap_err_with(|| format!("failed to order by {}", algo))?;
            (ordered, 0)
        };

        let total_gas: u64 = ordered_txs.iter().map(|tx| tx.gas_limit).sum();
        let estimated_value: u128 = ordered_txs
            .iter()
            .map(|tx| {
                let gas_price_str = if tx.tx_type == 2 {
                    &tx.max_fee_per_gas
                } else {
                    &tx.gas_price
                };
                let gas_price = if gas_price_str.starts_with("0x") {
                    u128::from_str_radix(gas_price_str.trim_start_matches("0x"), 16).unwrap_or(0)
                } else {
                    gas_price_str.parse::<u128>().unwrap_or(0)
                };
                (tx.gas_limit as u128) * gas_price
            })
            .sum();

        // Persistence to SQLite
        let algo_name = if algo == "egp" { "egp" } else { "profit" };
        store
            .insert_simulation_result(
                args.block,
                algo_name,
                ordered_txs.len(),
                total_gas,
                &format!("0x{:x}", estimated_value),
                "0x0",
            )
            .wrap_err("failed to insert simulation result")?;

        results.push((algo_name, ordered_txs, rejected));
    }

    // Print tx-by-tx table for single block
    for (algo_name, txs, rejected_count) in results {
        println!(
            "\n=== {} Algorithm: {} ===",
            algo_name.to_uppercase(),
            args.block
        );
        println!("Ordered: {}, Rejected: {}\n", txs.len(), rejected_count);

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
            let gas_price_gwei = if gas_price_str.starts_with("0x") {
                u128::from_str_radix(gas_price_str.trim_start_matches("0x"), 16).unwrap_or(0)
                    / 1_000_000_000
            } else {
                gas_price_str.parse::<u128>().unwrap_or(0) / 1_000_000_000
            };
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

async fn handle_status(_ctx: &AppContext, _args: StatusArgs) -> Result<()> {
    info!("status command stub invoked");
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .wrap_err_with(|| format!("failed to create data directory {}", path.display()))?;
    Ok(())
}
