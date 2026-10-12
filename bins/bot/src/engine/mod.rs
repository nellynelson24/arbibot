//! Trading engine: event loop and state.

mod args;
mod live;
mod maker;
mod market;
mod orders;
mod rebalance;
mod status;
mod switch;
mod taker;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

use account::{Bybit, Credentials, Fees, Okx, Scope};
use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, interval_at, sleep};
use venues::Venue;

use crate::book::{self, Book, Trade};
use crate::inventory::Inventory;
use crate::ledger::{Ledger, now_ms};
use crate::paper::Side;
use crate::selector;
use crate::telegram::{Notifier, Telegram};

pub use args::{Args, LiveArgs};
pub use live::live;
use maker::Maker;
use market::load_market;

/// Index 0 and 1 everywhere: wallets, books, fees, latencies.
const VENUES: [Venue; 2] = [Venue::Okx, Venue::Bybit];
const OKX: usize = 0;
const BYBIT: usize = 1;
const MAX_BOOK_AGE: Duration = Duration::from_secs(30);
const STATUS_EVERY: Duration = Duration::from_secs(60);
const STOP_FILE: &str = "STOP";
/// Pairs whose round-trip rebalance costs more than this share of capital are skipped.
const MAX_ROUTE_COST: f64 = 0.01;
/// Price slack when closing a remainder or selling a coin off.
const REMAINDER_SLIPPAGE: f64 = 0.01;
/// Price slack when buying a starting inventory.
const INVENTORY_SLIPPAGE: f64 = 0.003;
/// Room for the limit price above the best ask when sizing a trade to the money.
const SIZE_MARGIN: f64 = 0.01;

/// Clients of both venues.
struct Accounts {
    okx: Okx,
    bybit: Bybit,
}

/// The pair on both venues.
struct Market {
    base: String,
    quote: String,
    fees: [Fees; 2],
    /// Price step per venue.
    tick: [f64; 2],
    /// Quantity step and minimums valid on both venues.
    step: f64,
    min_qty: f64,
    min_notional: f64,
    /// Withdrawal fee from each venue; `None` = no shared network.
    base_fee: [Option<f64>; 2],
    quote_fee: [Option<f64>; 2],
}

/// Orders the books allow now: buy on venue `d`, sell on the other.
struct Plan {
    qty: f64,
    buy_limit: f64,
    sell_limit: f64,
    expected_bps: f64,
}

#[derive(Default)]
struct Stats {
    signals: u32,
    no_funds: u32,
    trades: u32,
    matched: u32,
    with_remainder: u32,
    unfilled: u32,
    pnl: f64,
    rebalances: u32,
    rebalance_cost: f64,
    switches: u32,
    switch_cost: f64,
    rebalance_trades: u32,
    rebalance_trade_pnl: f64,
    maker_fills: u32,
    maker_hedges: u32,
    maker_pnl: f64,
    hedge_misses: u32,
}

struct Engine {
    market: Market,
    /// Read-only keys.
    accounts: Accounts,
    /// Trading keys; `None` in the dry run.
    live: Option<Accounts>,
    order_seq: AtomicU32,
    /// Live: each order's details, journaled with its trade.
    orders: Mutex<Vec<serde_json::Value>>,
    slack_bps: f64,
    trade_rebalance: bool,
    rebalance_max_cost_bps: f64,
    last_trade_rebalance: Option<Instant>,
    http: reqwest::Client,
    data_dir: PathBuf,
    route_limit: f64,
    switching: bool,
    /// Better pair and its wins in a row.
    pending_switch: Option<(String, u32)>,
    telegram: Option<Notifier>,
    last_blocked: String,
    day_trades: u32,
    maker: Option<Maker>,
    /// OKX public trades, for maker orders.
    trades: Option<mpsc::UnboundedReceiver<Trade>>,
    books: [watch::Receiver<Book>; 2],
    latency: [Duration; 2],
    size: f64,
    min_bps: f64,
    daily_loss_limit: f64,
    transfer: Duration,
    inventory: Inventory,
    ledger: Ledger,
    stop_file: PathBuf,
    /// Per direction: a gap trades once, re-armed when it falls under the threshold.
    armed: [bool; 2],
    paused_until: Option<Instant>,
    halted: Option<String>,
    last_tick_ms: f64,
    last_report: Option<String>,
    day: String,
    day_pnl: f64,
    start_equity: f64,
    started: Instant,
    stats: Stats,
}

pub async fn dry_run(args: Args) -> Result<()> {
    run(args, None).await
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?)
}

async fn run(args: Args, live: Option<(Accounts, bool, bool)>) -> Result<()> {
    let http = http()?;
    let accounts = Accounts {
        okx: Okx::new(http.clone(), Credentials::load(Venue::Okx, Scope::Read)?),
        bybit: Bybit::new(http.clone(), Credentials::load(Venue::Bybit, Scope::Read)?),
    };
    let market = load_market(&args, &http, &accounts).await?;
    let (live, buy_inventory, confirmed) = match live {
        Some((trade, buy, yes)) => (Some(trade), buy, yes),
        None => (None, false, true),
    };
    let (books, trades) = open_books(&market, args.maker);
    let mid = wait_for_books(&books).await?;
    let inventory = Inventory::new(args.capital, mid);
    let mode = if live.is_some() { "live" } else { "dry-run" };
    let label = if live.is_some() {
        "NA ŻYWO"
    } else {
        "na sucho"
    };
    let telegram = match args.telegram {
        true => Some(Telegram::load()?.start(label, args.data_dir.join(STOP_FILE))),
        false => None,
    };
    let mut engine = Engine {
        start_equity: inventory.equity(mid),
        inventory,
        accounts,
        live,
        order_seq: AtomicU32::new(0),
        orders: Mutex::new(Vec::new()),
        slack_bps: args.slack_bps,
        trade_rebalance: args.trade_rebalance,
        rebalance_max_cost_bps: args.rebalance_max_cost_bps,
        last_trade_rebalance: None,
        http,
        data_dir: args.data_dir.clone(),
        route_limit: MAX_ROUTE_COST * args.capital,
        switching: args.switch,
        pending_switch: None,
        telegram,
        last_blocked: String::new(),
        day_trades: 0,
        maker: args
            .maker
            .then(|| Maker::new(args.maker_min_bps, args.requote_ms)),
        trades,
        books,
        latency: [
            Duration::from_millis(args.okx_latency_ms),
            Duration::from_millis(args.bybit_latency_ms),
        ],
        size: args.size,
        min_bps: args.min_bps,
        daily_loss_limit: args.daily_loss_limit,
        transfer: Duration::from_secs(args.transfer_mins * 60),
        ledger: Ledger::new(
            args.data_dir
                .join(if mode == "live" { "live" } else { "bot" }),
        )?,
        stop_file: args.data_dir.join(STOP_FILE),
        armed: [true; 2],
        paused_until: None,
        halted: None,
        last_tick_ms: now_ms(),
        last_report: None,
        day: selector::utc_date(now_ms()),
        day_pnl: 0.0,
        started: Instant::now(),
        stats: Stats::default(),
        market,
    };
    if engine.live.is_some() {
        engine.prepare_live(mid, buy_inventory, confirmed).await?;
        engine.start_equity = engine.inventory.equity(mid);
        // Waiting for TAK is not a stall.
        engine.last_tick_ms = now_ms();
    }
    engine.ledger.write(json!({
        "type": "start",
        "mode": mode,
        "wallets": engine.inventory.wallets.map(|w| [w.base, w.quote]),
        "pair": format!("{}/{}", engine.market.base, engine.market.quote),
        "capital": args.capital,
        "size": args.size,
        "min_bps": args.min_bps,
        "slack_bps": args.slack_bps,
        "trade_rebalance": args.trade_rebalance,
        "maker": args.maker,
        "maker_min_bps": args.maker_min_bps,
        "latency_ms": [args.okx_latency_ms, args.bybit_latency_ms],
        "fees_taker": [engine.market.fees[0].taker, engine.market.fees[1].taker],
    }))?;
    let started = format!(
        "Start {label}: {}/{q}, kapitał {:.2} {q}, transakcja {} {q}, próg {} bp, \
         dzienny limit straty {} {q}.",
        engine.market.base,
        engine.start_equity,
        args.size,
        args.min_bps,
        args.daily_loss_limit,
        q = engine.market.quote
    );
    println!(
        "{started} Zatrzymanie: Ctrl+C lub plik {}.\n",
        engine.stop_file.display()
    );
    engine.tell(started);

    let mut tick = interval(Duration::from_secs(1));
    let mut status = interval(STATUS_EVERY);
    status.tick().await;
    let review_every = Duration::from_secs_f64(args.review_hours.max(0.01) * 3600.0);
    let mut review = interval_at(tokio::time::Instant::now() + review_every, review_every);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    'run: loop {
        // New receivers after a change of pair.
        let [mut okx_changes, mut bybit_changes] = engine.books.clone();
        let mut trades = engine.trades.take();
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'run,
                Some(trade) = next_trade(&mut trades) => engine.on_trade(trade).await?,
                changed = okx_changes.changed() => {
                    changed.context("feed OKX zakończony")?;
                    engine.evaluate().await?;
                }
                changed = bybit_changes.changed() => {
                    changed.context("feed Bybit zakończony")?;
                    engine.evaluate().await?;
                }
                _ = tick.tick() => engine.tick()?,
                _ = status.tick() => engine.status()?,
                _ = review.tick(), if engine.switching => match engine.review().await {
                    Ok(true) => continue 'run,
                    Ok(false) => {}
                    Err(e) => println!("[{}] ocena par nieudana: {e:#}", clock()),
                },
            }
        }
    }
    let summary = engine.summary();
    if let Some(telegram) = engine.telegram.take() {
        telegram.close().await;
    }
    summary
}

impl Engine {
    /// On every book change: hedges, taker, rebalance, maker quotes.
    async fn evaluate(&mut self) -> Result<()> {
        if self.halted.is_some() || self.paused_until.is_some_and(|t| Instant::now() < t) {
            if let Some(maker) = &mut self.maker {
                maker.quotes = [None, None];
            }
            return Ok(());
        }
        if self.maker.is_some() {
            for side in [Side::Buy, Side::Sell] {
                self.hedge(side).await?;
            }
        }
        self.taker().await?;
        if self.trade_rebalance {
            self.rebalance_by_trade().await?;
        }
        self.refresh_quotes();
        Ok(())
    }

    fn mid(&self) -> Option<f64> {
        let (sum, n) = self
            .books
            .iter()
            .filter_map(|b| b.borrow().mid())
            .fold((0.0, 0), |(sum, n), mid| (sum + mid, n + 1));
        (n > 0).then(|| sum / f64::from(n))
    }

    fn tell(&self, text: impl Into<String>) {
        if let Some(telegram) = &self.telegram {
            telegram.send(text);
        }
    }
}

async fn wait_for_books(books: &[watch::Receiver<Book>; 2]) -> Result<f64> {
    for _ in 0..60 {
        let mids: Vec<f64> = books.iter().filter_map(|b| b.borrow().mid()).collect();
        if mids.len() == 2 {
            return Ok(mids.iter().sum::<f64>() / 2.0);
        }
        sleep(Duration::from_secs(1)).await;
    }
    bail!("brak książek obu giełd po 60 s")
}

/// Both books, plus OKX public trades when maker orders need them.
fn open_books(
    market: &Market,
    with_trades: bool,
) -> (
    [watch::Receiver<Book>; 2],
    Option<mpsc::UnboundedReceiver<Trade>>,
) {
    let bybit = book::spawn(Venue::Bybit, &market.base, &market.quote);
    if with_trades {
        let (okx, trades) = book::spawn_with_trades(&market.base, &market.quote);
        ([okx, bybit], Some(trades))
    } else {
        (
            [book::spawn(Venue::Okx, &market.base, &market.quote), bybit],
            None,
        )
    }
}

async fn next_trade(trades: &mut Option<mpsc::UnboundedReceiver<Trade>>) -> Option<Trade> {
    match trades {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn clock() -> String {
    DateTime::from_timestamp_millis(now_ms() as i64)
        .map(|t| t.format("%H:%M:%S UTC").to_string())
        .unwrap_or_default()
}
