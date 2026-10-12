//! OKX EEA private API.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::{Balance, Credentials, Fees, KeyRights, Network, OrderFill, Rules, Side, num, sign};

/// API host for accounts of OKX's EEA entity (my.okx.com).
const BASE: &str = "https://eea.okx.com";

pub struct Okx {
    http: Client,
    creds: Credentials,
}

impl Okx {
    /// Public and cheap: requested now and then to keep the connection open.
    pub const TIME_URL: &str = "https://eea.okx.com/api/v5/public/time";

    pub fn new(http: Client, creds: Credentials) -> Self {
        Okx { http, creds }
    }

    /// Balances of the trading and the funding account (deposits land in funding).
    pub async fn balances(&self) -> Result<Vec<Balance>> {
        let trading = self.get("/api/v5/account/balance").await?;
        let funding = self.get("/api/v5/asset/balances").await?;
        let mut out = Vec::new();
        for d in trading[0]["details"].as_array().into_iter().flatten() {
            out.push(Balance {
                account: "trading",
                asset: d["ccy"].as_str().unwrap_or_default().to_owned(),
                total: num(&d["cashBal"]).unwrap_or(0.0),
                available: num(&d["availBal"]).unwrap_or(0.0),
            });
        }
        for d in funding.as_array().into_iter().flatten() {
            out.push(Balance {
                account: "funding",
                asset: d["ccy"].as_str().unwrap_or_default().to_owned(),
                total: num(&d["bal"]).unwrap_or(0.0),
                available: num(&d["availBal"]).unwrap_or(0.0),
            });
        }
        Ok(out)
    }

    /// Account spot fees for a pair. OKX reports paid fees as negative numbers and has
    /// separate fields for USDC pairs.
    pub async fn fees(&self, base: &str, quote: &str) -> Result<Fees> {
        let path = format!("/api/v5/account/trade-fee?instType=SPOT&instId={base}-{quote}");
        let data = self.get(&path).await?;
        let d = &data[0];
        let usdc = quote == "USDC" && num(&d["takerUSDC"]).is_some();
        let (maker, taker) = if usdc {
            (&d["makerUSDC"], &d["takerUSDC"])
        } else {
            (&d["maker"], &d["taker"])
        };
        Ok(Fees {
            maker: -num(maker).context("OKX: brak prowizji maker")?,
            taker: -num(taker).context("OKX: brak prowizji taker")?,
        })
    }

    pub async fn rules(&self, base: &str, quote: &str) -> Result<Rules> {
        let path = format!("/api/v5/public/instruments?instType=SPOT&instId={base}-{quote}");
        let body = self
            .http
            .get(format!("{BASE}{path}"))
            .send()
            .await?
            .text()
            .await?;
        let data = data(&path, &body)?;
        let d = &data[0];
        Ok(Rules {
            tick: num(&d["tickSz"]).context("OKX: brak tickSz")?,
            step: num(&d["lotSz"]).context("OKX: brak lotSz")?,
            min_qty: num(&d["minSz"]).context("OKX: brak minSz")?,
            min_notional: None,
        })
    }

    pub async fn networks(&self, asset: &str) -> Result<Vec<Network>> {
        let data = self
            .get(&format!("/api/v5/asset/currencies?ccy={asset}"))
            .await?;
        Ok(data
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| Network {
                chain: d["chain"].as_str().unwrap_or_default().to_owned(),
                can_deposit: d["canDep"] == true,
                can_withdraw: d["canWd"] == true,
                withdraw_fee: num(&d["minFee"]).or_else(|| num(&d["fee"])),
                withdraw_min: num(&d["minWd"]),
                confirmations: num(&d["minDepArrivalConfirm"]).map(|c| c as u32),
            })
            .collect())
    }

    /// Rights of this key (`perm` lists e.g. `read_only,trade,withdraw`).
    pub async fn key_rights(&self) -> Result<KeyRights> {
        let data = self.get("/api/v5/account/config").await?;
        let perm = data[0]["perm"].as_str().unwrap_or_default();
        Ok(KeyRights {
            trade: perm.contains("trade"),
            withdraw: perm.contains("withdraw"),
        })
    }

    /// Spot limit IOC: fills up to `qty` at `price` or better, cancels the rest.
    /// `client_id`: letters and digits, at most 32.
    pub async fn place_ioc(
        &self,
        base: &str,
        quote: &str,
        side: Side,
        qty: &str,
        price: &str,
        client_id: &str,
    ) -> Result<()> {
        let body = json!({
            "instId": format!("{base}-{quote}"),
            "tdMode": "cash",
            "clOrdId": client_id,
            "side": match side { Side::Buy => "buy", Side::Sell => "sell" },
            "ordType": "ioc",
            "px": price,
            "sz": qty,
        });
        let data = self.post("/api/v5/trade/order", &body.to_string()).await?;
        if data[0]["sCode"] != "0" {
            bail!(
                "OKX odrzucił zlecenie: {} {}",
                data[0]["sCode"],
                data[0]["sMsg"]
            );
        }
        Ok(())
    }

    /// The fill of an order once it is final; `None` while it is still live.
    pub async fn order(
        &self,
        base: &str,
        quote: &str,
        client_id: &str,
    ) -> Result<Option<OrderFill>> {
        let path = format!("/api/v5/trade/order?instId={base}-{quote}&clOrdId={client_id}");
        let data = self.get(&path).await?;
        let d = &data[0];
        match d["state"].as_str() {
            Some("filled" | "canceled" | "mmp_canceled") => Ok(Some(OrderFill {
                qty: num(&d["accFillSz"]).unwrap_or(0.0),
                avg_price: num(&d["avgPx"]).unwrap_or(0.0),
            })),
            Some(_) => Ok(None),
            None => bail!("OKX: brak zlecenia {client_id}"),
        }
    }

    /// Signed GET of a private endpoint; returns the `data` field.
    async fn get(&self, path: &str) -> Result<Value> {
        let body = self.signed(reqwest::Method::GET, path, "").await?;
        data(path, &body)
    }

    /// Signed POST with a JSON body; returns the `data` field.
    async fn post(&self, path: &str, json: &str) -> Result<Value> {
        let body = self.signed(reqwest::Method::POST, path, json).await?;
        data(path, &body)
    }

    async fn signed(&self, method: reqwest::Method, path: &str, json: &str) -> Result<String> {
        let timestamp = sign::iso_ms(sign::now_ms());
        let signature = sign::okx(&self.creds.secret, &timestamp, method.as_str(), path, json);
        let passphrase = self
            .creds
            .passphrase
            .as_deref()
            .context("OKX wymaga passphrase")?;
        let mut request = self
            .http
            .request(method, format!("{BASE}{path}"))
            .header("OK-ACCESS-KEY", &self.creds.api_key)
            .header("OK-ACCESS-SIGN", signature)
            .header("OK-ACCESS-TIMESTAMP", timestamp)
            .header("OK-ACCESS-PASSPHRASE", passphrase);
        if !json.is_empty() {
            request = request
                .header("Content-Type", "application/json")
                .body(json.to_owned());
        }
        Ok(request.send().await?.text().await?)
    }
}

fn data(path: &str, body: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(body)
        .with_context(|| format!("OKX {path}: odpowiedź nie jest JSON-em"))?;
    if v["code"] != "0" {
        // Order endpoints put the reason in the first item.
        let item = &v["data"][0];
        bail!(
            "OKX {path}: kod {} {} {} {}",
            v["code"],
            v["msg"],
            item["sCode"].as_str().unwrap_or_default(),
            item["sMsg"].as_str().unwrap_or_default()
        );
    }
    Ok(v["data"].clone())
}
