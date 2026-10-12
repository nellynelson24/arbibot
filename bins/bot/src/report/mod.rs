//! `bot report`.

mod compare;
mod live;
mod market;
mod stats;
mod strategy;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::DateTime;
use serde_json::Value;

use crate::ledger::now_ms;
use crate::telegram::Telegram;
use compare::{agreement_section, variants_section};
use live::live_section;
use market::market_section;
use stats::pl_time;
use strategy::{calibration_section, maker_section};

const HOUR_MS: f64 = 3_600_000.0;
/// The same signal in two runs: same direction, this close in time.
const SAME_SIGNAL_MS: f64 = 5_000.0;
/// Below this many cases a rule stays silent.
const MIN_CASES: usize = 8;
/// Telegram refuses longer messages.
const TELEGRAM_LIMIT: usize = 3_900;

#[derive(clap::Args)]
pub struct Args {
    /// Okno raportu w godzinach, wstecz od teraz
    #[arg(long, default_value_t = 24.0)]
    hours: f64,
    /// Katalog danych skanera i botów
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,
    /// Wyślij skrót raportu na Telegram (Tobie i widzom)
    #[arg(long)]
    telegram: bool,
}

pub struct Report {
    /// Markdown, saved to a file.
    pub full: String,
    /// The essentials for Telegram.
    pub short: String,
}

pub async fn run(args: Args) -> Result<()> {
    let until = now_ms();
    let report = build(&args.data_dir, until - args.hours * HOUR_MS, until)?;
    let path = save(&args.data_dir, &report, until)?;
    println!("{}\nZapisano: {}", report.full, path.display());
    if args.telegram {
        Telegram::load()?.send_report(&report.short).await;
    }
    Ok(())
}

pub fn save(data_dir: &Path, report: &Report, until: f64) -> Result<PathBuf> {
    let dir = data_dir.join("reports");
    fs::create_dir_all(&dir).with_context(|| format!("katalog {}", dir.display()))?;
    let stamp = DateTime::from_timestamp_millis(until as i64)
        .map(|t| t.format("%Y-%m-%d_%H%M").to_string())
        .unwrap_or_default();
    let path = dir.join(format!("{stamp}.md"));
    fs::write(&path, &report.full).with_context(|| format!("zapis {}", path.display()))?;
    Ok(path)
}

/// One bot's journal: its records inside the window and its latest settings.
struct Run {
    label: String,
    live: bool,
    /// The last start at or before the end of the window.
    start: Option<Value>,
    rows: Vec<Value>,
}

impl Run {
    fn of(&self, kind: &str) -> Vec<&Value> {
        self.rows.iter().filter(|v| v["type"] == kind).collect()
    }

    fn trades(&self) -> Vec<&Value> {
        self.of("trade")
    }

    fn setting(&self, key: &str) -> Option<f64> {
        self.start.as_ref().and_then(|s| s[key].as_f64())
    }

    fn pair(&self) -> Option<String> {
        self.start
            .as_ref()
            .and_then(|s| s["pair"].as_str())
            .map(str::to_owned)
    }

    fn settings(&self) -> String {
        let Some(s) = &self.start else {
            return "-".to_owned();
        };
        let mut parts = vec![
            s["pair"].as_str().unwrap_or("?").to_owned(),
            format!("próg {}", num(s, "min_bps")),
            format!("{} USD", num(s, "size")),
        ];
        if num(s, "slack_bps") > 0.0 {
            parts.push(format!("zapas {}", num(s, "slack_bps")));
        }
        if s["maker"] == true {
            parts.push(format!("maker {}", num(s, "maker_min_bps")));
        }
        if let Some(latency) = s["latency_ms"].as_array()
            && !self.live
        {
            parts.push(format!(
                "opóźn. {}/{}",
                latency.first().and_then(Value::as_f64).unwrap_or(0.0),
                latency.get(1).and_then(Value::as_f64).unwrap_or(0.0)
            ));
        }
        parts.join(", ")
    }
}

fn num(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(0.0)
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

fn ts(v: &Value) -> f64 {
    num(v, "ts_ms")
}

fn executed(trade: &Value) -> bool {
    text(trade, "outcome") != "brak wypełnienia"
}

pub fn build(data_dir: &Path, since: f64, until: f64) -> Result<Report> {
    let runs: Vec<Run> = sources(data_dir)
        .into_iter()
        .filter_map(|(label, dir, live)| load(label, &dir, live, since, until))
        .collect();
    let live = runs.iter().find(|r| r.live);
    let mut full = String::new();
    let mut short = String::new();
    let mut advice = Vec::new();

    let _ = writeln!(
        full,
        "# Raport federbot: {} → {} (czas polski, {:.0} h)\n",
        pl_time(since, "%d.%m %H:%M"),
        pl_time(until, "%d.%m %H:%M"),
        (until - since) / HOUR_MS
    );
    let _ = writeln!(
        short,
        "Raport {:.0} h do {}",
        (until - since) / HOUR_MS,
        pl_time(until, "%d.%m %H:%M")
    );

    if let Some(live) = live {
        live_section(live, &mut full, &mut short, &mut advice);
    } else {
        let _ = writeln!(full, "## Na żywo\n\nBrak handlu na żywo w tym oknie.\n");
    }
    variants_section(&runs, &mut full, &mut short, &mut advice);
    if let Some(live) = live {
        agreement_section(live, &runs, &mut full);
    }
    maker_section(&runs, &mut full, &mut advice);
    calibration_section(&runs, &mut full, &mut advice);
    market_section(
        data_dir,
        live.or(runs.first()),
        since,
        until,
        &mut full,
        &mut advice,
    );

    let _ = writeln!(full, "## Wnioski\n");
    if advice.is_empty() {
        let _ = writeln!(full, "- Za mało danych na wnioski w tym oknie.");
    }
    for a in &advice {
        let _ = writeln!(full, "- {a}");
    }
    let _ = writeln!(short, "\nWnioski:");
    for a in advice.iter().take(6) {
        let _ = writeln!(short, "• {a}");
    }
    if advice.is_empty() {
        let _ = writeln!(short, "• za mało danych");
    }
    if short.chars().count() > TELEGRAM_LIMIT {
        short = short.chars().take(TELEGRAM_LIMIT).collect::<String>() + "…";
    }
    Ok(Report { full, short })
}

/// The live journal, the main dry run and every `ab-*` experiment.
fn sources(data_dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = vec![
        ("na żywo".to_owned(), data_dir.join("live"), true),
        ("główny".to_owned(), data_dir.join("bot"), false),
    ];
    let mut ab: Vec<(String, PathBuf, bool)> = fs::read_dir(data_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("ab-")
                .then(|| (name, e.path().join("bot"), false))
        })
        .collect();
    ab.sort_by(|a, b| a.0.cmp(&b.0));
    out.extend(ab);
    out
}

fn load(label: String, dir: &Path, live: bool, since: f64, until: f64) -> Option<Run> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    let mut start = None;
    let mut rows = Vec::new();
    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        for v in content
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        {
            let t = ts(&v);
            if t > until {
                continue;
            }
            if v["type"] == "start" {
                start = Some(v.clone());
            }
            if t >= since {
                rows.push(v);
            }
        }
    }
    (!rows.is_empty()).then_some(Run {
        label,
        live,
        start,
        rows,
    })
}
