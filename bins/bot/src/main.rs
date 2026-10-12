//! Trading bot CLI.

mod book;
mod check;
mod edge;
mod engine;
mod inventory;
mod ledger;
mod paper;
mod report;
mod selector;
mod telegram;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};

/// Bot handlowy taker–taker OKX EEA ↔ Bybit EU (plan: BOT_PLAN.md).
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Faza 1: odczyt kont — salda, prowizje, reguły, sieci wypłat
    Check(check::Args),
    /// Na żywo: zysk po prowizjach dla kilku kwot, z głębokości książek obu giełd
    Edge(edge::Args),
    /// Faza 2: cały bot bez wysyłania zleceń (symulowane wypełnienia, salda, rebalans)
    DryRun(engine::Args),
    /// Faza 4: handel prawdziwymi pieniędzmi (klucze handlowe bez prawa wypłat)
    Live(engine::LiveArgs),
    /// Telegram: konfiguracja i wiadomość testowa (token i czat w pęku kluczy)
    Telegram(telegram::Args),
    /// Raport: handel na żywo, wszystkie dry-runy i rynek ze skanera, z wnioskami
    Report(report::Args),
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("nie udało się ustawić dostawcy kryptografii rustls"))?;

    match Cli::parse().command {
        Command::Check(args) => check::run(args).await,
        Command::Edge(args) => edge::run(args).await,
        Command::DryRun(args) => engine::dry_run(args).await,
        Command::Live(args) => engine::live(args).await,
        Command::Telegram(args) => telegram::setup(args).await,
        Command::Report(args) => report::run(args).await,
    }
}
