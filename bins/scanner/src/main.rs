//! Live cross-venue scanner CLI.

mod clock;
mod engine;
mod feed;
mod funding;
mod maker;
mod report;
mod select;
mod transit;
mod universe;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use tokio::sync::mpsc;
use venues::Venue;

use universe::Universe;

const CHANNEL_CAPACITY: usize = 65_536;

/// Skaner okazji arbitrażowych na żywo (Faza 1).
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Słucha giełd i zapisuje okazje do <data-dir>/opportunities/RRRR-MM-DD.jsonl
    Run(ScanArgs),
    /// Wybrane pary z bliska: każdy tick do pliku i podgląd na żywo
    Watch(WatchArgs),
    /// Raport dzienny z prognozą zarobków
    Report(report::ReportArgs),
}

#[derive(Args)]
struct ScanArgs {
    /// Giełdy, np. okx,kraken (domyślnie wszystkie)
    #[arg(long, value_enum, value_delimiter = ',')]
    venues: Vec<Venue>,
    /// Waluty kwotowania
    #[arg(long, value_delimiter = ',', default_value = "EUR,USDC")]
    quotes: Vec<String>,
    /// Próg zysku po prowizjach taker, od którego liczy się okazja (1 bp = 0,01%)
    #[arg(long, default_value_t = 0.0)]
    min_net_bps: f64,
    /// Katalog na dane
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    /// Co ile sekund wypisywać podsumowanie
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    status_secs: u64,
    /// Wyłącz symulację market makingu z zabezpieczeniem
    #[arg(long)]
    no_maker: bool,
    /// Kapitał symulowany na jeden rynek (para + 2 giełdy), w walucie kwotowania
    #[arg(long, default_value_t = 1_000.0)]
    capital: f64,
    /// Minimalna marża zlecenia maker po prowizjach maker + taker (bp)
    #[arg(long, default_value_t = 5.0)]
    maker_min_bps: f64,
    /// Minimalny odstęp między przestawieniami zlecenia (ms)
    #[arg(long, default_value_t = 1_000)]
    requote_ms: u64,
    /// Wielkość zlecenia jako ułamek najlepszej oferty na giełdzie zabezpieczenia
    #[arg(long, default_value_t = 0.5)]
    hedge_depth_factor: f64,
    /// Najmniejsze zlecenie (waluta kwotowania)
    #[arg(long, default_value_t = 10.0)]
    min_order: f64,
    /// Opóźnienie zabezpieczenia po wykonaniu zlecenia maker (ms)
    #[arg(long, default_value_t = 150)]
    hedge_delay_ms: u64,
    /// Wyłącz sprawdzanie ceny sprzedaży po czasie przelewu (strategia cykliczna)
    #[arg(long)]
    no_transit: bool,
    /// Strategia cykliczna: śledź okazje z zyskiem po prowizjach co najmniej tyle (bp)
    #[arg(long, default_value_t = 10.0)]
    transit_min_bps: f64,
    /// Czas reakcji bota: okazja musi trwać co najmniej tyle, żeby kupić (ms)
    #[arg(long, default_value_t = 100)]
    reaction_ms: u64,
}

#[derive(Args)]
struct WatchArgs {
    /// Pary, np. ZRC/EUR,LUNA/EUR (domyślnie: najbardziej opłacalne w chwili startu)
    #[arg(long, value_delimiter = ',')]
    pairs: Vec<String>,
    /// Ile par wybrać automatycznie
    #[arg(long, default_value_t = 5)]
    top: usize,
    /// Co ile sekund odświeżać podgląd
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    view_secs: u64,
    #[command(flatten)]
    scan: ScanArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("nie udało się ustawić dostawcy kryptografii rustls"))?;

    match Cli::parse().command {
        Command::Run(args) => scan(args, None).await,
        Command::Watch(args) => {
            let WatchArgs {
                pairs,
                top,
                view_secs,
                scan: scan_args,
            } = args;
            scan(scan_args, Some((pairs, top, view_secs))).await
        }
        Command::Report(args) => report::run(&args),
    }
}

/// `watch`: (pair names, how many to pick if none, view interval in seconds).
async fn scan(args: ScanArgs, watch: Option<(Vec<String>, usize, u64)>) -> Result<()> {
    let venues = if args.venues.is_empty() {
        Venue::ALL.to_vec()
    } else {
        args.venues
    };
    let quotes: Vec<String> = args.quotes.iter().map(|q| q.to_uppercase()).collect();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let universe = Universe::discover(&client, &venues, &quotes).await?;
    let per_venue: Vec<String> = venues
        .iter()
        .map(|v| format!("{} {}", v.name(), universe.listings(*v).len()))
        .collect();
    println!(
        "Pary na co najmniej 2 giełdach: {} ({})",
        universe.pairs.len(),
        per_venue.join(", ")
    );

    let watch = match watch {
        None => None,
        Some((names, top, view_secs)) => {
            let pairs = if names.is_empty() {
                let picks = select::top_pairs(&client, &universe, &venues, top).await;
                println!("Najbardziej opłacalne w tej chwili (po prowizjach taker):");
                for (route, signal) in &picks {
                    println!(
                        "  {:<12} {} → {} {:+.1} bp",
                        universe.pairs[route.pair].to_string(),
                        route.buy.name(),
                        route.sell.name(),
                        signal.net_bps
                    );
                }
                picks.into_iter().map(|(route, _)| route.pair).collect()
            } else {
                select::named_pairs(&universe, &names)?
            };
            Some(engine::Watch {
                pairs,
                every: Duration::from_secs(view_secs),
            })
        }
    };
    let ticks_note = if watch.is_some() { ", ticks/" } else { "" };
    println!(
        "Zapis: {}/opportunities/{ticks_note}. Zatrzymanie: Ctrl+C\n",
        args.data_dir.display()
    );

    tokio::spawn(funding::run(client.clone(), args.data_dir.clone()));
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    for &venue in &venues {
        let listings: Vec<_> = universe
            .listings(venue)
            .iter()
            .filter(|l| watch.as_ref().is_none_or(|w| w.pairs.contains(&l.pair)))
            .cloned()
            .collect();
        if !listings.is_empty() {
            tokio::spawn(feed::run(venue, listings, client.clone(), tx.clone()));
        }
    }
    drop(tx);

    let cfg = engine::Config {
        min_net_bps: args.min_net_bps,
        data_dir: args.data_dir,
        status_every: Duration::from_secs(args.status_secs),
        watch,
        maker: (!args.no_maker).then(|| maker::Config {
            capital: args.capital,
            min_edge_bps: args.maker_min_bps,
            requote: Duration::from_millis(args.requote_ms),
            hedge_depth_factor: args.hedge_depth_factor,
            min_order: args.min_order,
            hedge_delay: Duration::from_millis(args.hedge_delay_ms),
        }),
        transit: (!args.no_transit).then(|| transit::Config {
            min_net_bps: args.transit_min_bps,
            reaction: Duration::from_millis(args.reaction_ms),
        }),
    };
    engine::run(rx, &universe, cfg).await
}
