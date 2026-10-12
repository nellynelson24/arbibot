//! Private exchange APIs: keys, signing, balances, fees, orders.

mod bybit;
mod credentials;
mod okx;
mod sign;

pub use bybit::Bybit;
pub use credentials::{Credentials, Scope, secret};
pub use okx::Okx;

/// Balance of one asset in one account of a venue.
#[derive(Debug, Clone)]
pub struct Balance {
    /// Account inside the venue, e.g. `trading` or `funding`.
    pub account: &'static str,
    pub asset: String,
    pub total: f64,
    pub available: f64,
}

/// Fees charged as a fraction of the traded value (0.001 = 0.1%); negative = rebate.
#[derive(Debug, Clone, Copy)]
pub struct Fees {
    pub maker: f64,
    pub taker: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// What a final order filled: base quantity and its average price.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrderFill {
    pub qty: f64,
    pub avg_price: f64,
}

/// What an API key may do, as the venue reports it.
#[derive(Debug, Clone, Copy)]
pub struct KeyRights {
    pub trade: bool,
    pub withdraw: bool,
}

/// Order rules of a spot instrument.
#[derive(Debug, Clone, Copy)]
pub struct Rules {
    /// Price step.
    pub tick: f64,
    /// Quantity step (base asset).
    pub step: f64,
    pub min_qty: f64,
    /// Smallest order value in quote currency, where the venue sets one.
    pub min_notional: Option<f64>,
}

/// A network an asset can be deposited and withdrawn on.
#[derive(Debug, Clone)]
pub struct Network {
    /// The venue's own name of the network.
    pub chain: String,
    pub can_deposit: bool,
    pub can_withdraw: bool,
    /// In units of the asset.
    pub withdraw_fee: Option<f64>,
    pub withdraw_min: Option<f64>,
    pub confirmations: Option<u32>,
}

impl Network {
    /// Lowercase chain name without the asset prefix and with aliases folded, so it
    /// matches across venues (`USDC-Solana` = `SOL`).
    pub fn key(&self) -> String {
        let name = self.chain.to_lowercase();
        let name = name
            .split_once('-')
            .map_or(name.as_str(), |(_, chain)| chain);
        let name = name.trim().replace([' ', '_'], "");
        match name.as_str() {
            "sol" | "solana" => "solana",
            "eth" | "erc20" | "ethereum" => "ethereum",
            "bsc" | "bep20" | "bnbsmartchain" | "bnbchain" => "bsc",
            "trx" | "trc20" | "tron" => "tron",
            "arbi" | "arb" | "arbitrum" | "arbitrumone" => "arbitrum",
            "op" | "optimism" => "optimism",
            "matic" | "polygon" | "polygonpos" => "polygon",
            "avaxc" | "avalanchec" | "avalanchec-chain" | "avalanchecchain" => "avalanche-c",
            "base" => "base",
            "kaia" | "klay" | "klaytn" => "kaia",
            other => return other.to_owned(),
        }
        .to_owned()
    }
}

/// Number sent as JSON number or string; empty strings are `None`.
pub(crate) fn num(v: &serde_json::Value) -> Option<f64> {
    let x = match v {
        serde_json::Value::String(s) if !s.is_empty() => s.parse().ok()?,
        serde_json::Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    x.is_finite().then_some(x)
}
