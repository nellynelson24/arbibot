//! Command-line options.

use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Para, np. STRK/USDC; domyślnie najlepsza ze skanera
    #[arg(long)]
    pub(super) pair: Option<String>,
    /// Na sucho: kapitał (waluta kwotowania). Na żywo liczą się prawdziwe salda
    #[arg(long, default_value_t = 100.0)]
    pub(super) capital: f64,
    /// Wielkość transakcji (waluta kwotowania)
    #[arg(long, default_value_t = 10.0)]
    pub(super) size: f64,
    /// Próg zysku po prowizjach (bp; 1 bp = 0,01%)
    #[arg(long, default_value_t = 30.0, allow_negative_numbers = true)]
    pub(super) min_bps: f64,
    /// Zapas ceny na nogę (bp), najwyżej ćwierć oczekiwanego zysku
    #[arg(long, default_value_t = 0.0)]
    pub(super) slack_bps: f64,
    /// Na sucho: rebalans przez sprzedaż i kupno zamiast przelewów
    #[arg(long)]
    pub(super) trade_rebalance: bool,
    /// Najwyższy koszt rebalansu handlem (bp)
    #[arg(long, default_value_t = 45.0)]
    pub(super) rebalance_max_cost_bps: f64,
    /// Na sucho: opóźnienie zleceń na OKX (ms)
    #[arg(long, default_value_t = 100)]
    pub(super) okx_latency_ms: u64,
    /// Na sucho: opóźnienie zleceń na Bybit (ms)
    #[arg(long, default_value_t = 200)]
    pub(super) bybit_latency_ms: u64,
    /// Dzienny limit straty (waluta kwotowania)
    #[arg(long, default_value_t = 2.0)]
    pub(super) daily_loss_limit: f64,
    /// Na sucho: czas przelewu przy rebalansie (min)
    #[arg(long, default_value_t = 10)]
    pub(super) transfer_mins: u64,
    /// Katalog danych
    #[arg(long, default_value = "data")]
    pub(super) data_dir: PathBuf,
    /// Na sucho: automatyczna zmiana pary
    #[arg(long)]
    pub(super) switch: bool,
    /// Co ile godzin oceniać pary (z --switch)
    #[arg(long, default_value_t = 24.0)]
    pub(super) review_hours: f64,
    /// Powiadomienia i komendy /stan, /stop przez Telegram
    #[arg(long)]
    pub(super) telegram: bool,
    /// Na sucho: zlecenia maker na OKX zabezpieczane na Bybit
    #[arg(long)]
    pub(super) maker: bool,
    /// Minimalna marża maker (bp)
    #[arg(long, default_value_t = 10.0, allow_negative_numbers = true)]
    pub(super) maker_min_bps: f64,
    /// Minimalny odstęp przestawień zlecenia maker (ms)
    #[arg(long, default_value_t = 1_000)]
    pub(super) requote_ms: u64,
}

#[derive(clap::Args)]
pub struct LiveArgs {
    #[command(flatten)]
    pub(super) common: Args,
    /// Na starcie dokup brakującą monetę
    #[arg(long)]
    pub(super) buy_inventory: bool,
    /// Bez pytania o potwierdzenie
    #[arg(long)]
    pub(super) yes: bool,
}
