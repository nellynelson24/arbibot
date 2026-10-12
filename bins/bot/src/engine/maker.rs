//! Maker orders on OKX, hedged on Bybit (dry run).

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;

use super::orders::{floor_to, side_id};
use super::{BYBIT, Engine, MAX_BOOK_AGE, OKX, clock};
use crate::book::{Book, Trade};
use crate::ledger::now_ms;
use crate::paper::Side;

/// Price slack for the hedge, which must go through.
const HEDGE_SLIPPAGE: f64 = 0.005;
/// Quote only while the hedge book is this fresh.
const MAX_HEDGE_AGE: Duration = Duration::from_secs(5);

/// A resting maker order.
pub(super) struct Resting {
    price: f64,
    /// Unfilled base quantity.
    qty: f64,
    /// Base volume queued at our price before us.
    queue_ahead: f64,
}

/// Maker orders: index 0 buys, 1 sells on OKX.
#[derive(Default)]
pub(super) struct Maker {
    min_bps: f64,
    requote: Duration,
    pub(super) quotes: [Option<Resting>; 2],
    last_quoted: [Option<Instant>; 2],
    /// Base filled and not yet hedged, and the quote it moved.
    unhedged: [f64; 2],
    unhedged_cash: [f64; 2],
}

impl Maker {
    pub(super) fn new(min_bps: f64, requote_ms: u64) -> Self {
        Maker {
            min_bps,
            requote: Duration::from_millis(requote_ms),
            ..Maker::default()
        }
    }

    /// Same settings, no quotes or leftovers (after a change of pair).
    pub(super) fn reset(&mut self) {
        *self = Maker {
            min_bps: self.min_bps,
            requote: self.requote,
            ..Maker::default()
        };
    }
}

fn index(side: Side) -> usize {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}

impl Engine {
    /// Keeps, moves, places or cancels the maker orders after a book change.
    pub(super) fn refresh_quotes(&mut self) {
        let Some(maker) = &self.maker else {
            return;
        };
        let (min_bps, requote) = (maker.min_bps, maker.requote);
        let mut next: [Option<Option<Resting>>; 2] = [None, None];
        let mut placed = [false; 2];
        {
            let okx = self.books[OKX].borrow();
            let bybit = self.books[BYBIT].borrow();
            let fresh = okx.age().is_some_and(|a| a <= MAX_BOOK_AGE)
                && bybit.age().is_some_and(|a| a <= MAX_HEDGE_AGE);
            for side in [Side::Buy, Side::Sell] {
                let i = index(side);
                let best = match side {
                    Side::Buy => okx.bids.first(),
                    Side::Sell => okx.asks.first(),
                };
                let Some(&(best, best_qty)) = best.filter(|_| fresh) else {
                    next[i] = Some(None);
                    continue;
                };
                let maker = self.maker.as_ref().expect("checked above");
                let can_place = maker.last_quoted[i].is_none_or(|t| t.elapsed() >= requote);
                let mut current = maker.quotes[i]
                    .as_ref()
                    .map(|q| (q.price, q.qty, q.queue_ahead));
                // A better price than ours appeared: follow when allowed.
                if let Some((price, ..)) = current {
                    let behind = match side {
                        Side::Buy => best > price,
                        Side::Sell => best < price,
                    };
                    if behind && can_place {
                        current = None;
                    }
                }
                let price = current.map_or(best, |q| q.0);
                let qty = floor_to(self.size / price, self.market.step);
                let worth = self
                    .maker_edge(side, price, qty, &bybit)
                    .is_some_and(|edge| edge >= min_bps)
                    && self.maker_funded(side, price, qty, &bybit)
                    && qty >= self.market.min_qty
                    && qty * price >= self.market.min_notional;
                next[i] = Some(match (worth, current) {
                    (false, _) => None,
                    (true, Some((price, qty, queue_ahead))) => Some(Resting {
                        price,
                        qty,
                        queue_ahead,
                    }),
                    (true, None) if can_place => {
                        placed[i] = true;
                        Some(Resting {
                            price: best,
                            qty,
                            queue_ahead: best_qty,
                        })
                    }
                    (true, None) => None,
                });
            }
        }
        let maker = self.maker.as_mut().expect("checked above");
        for i in 0..2 {
            if let Some(quote) = next[i].take() {
                maker.quotes[i] = quote;
            }
            if placed[i] {
                maker.last_quoted[i] = Some(Instant::now());
            }
        }
    }

    /// Return (bp) of a maker fill at `price` hedged now on Bybit, after fees.
    fn maker_edge(&self, side: Side, price: f64, qty: f64, bybit: &Book) -> Option<f64> {
        let (maker_fee, taker_fee) = (self.market.fees[OKX].maker, self.market.fees[BYBIT].taker);
        let net = match side {
            Side::Sell => price * (1.0 - maker_fee) / (bybit.buy_fill(qty)?.0 * (1.0 + taker_fee)),
            Side::Buy => bybit.sell_fill(qty)?.0 * (1.0 - taker_fee) / (price * (1.0 + maker_fee)),
        };
        Some((net - 1.0) * 1e4)
    }

    fn maker_funded(&self, side: Side, price: f64, qty: f64, bybit: &Book) -> bool {
        let [okx, hedge] = self.inventory.wallets;
        let hedge_fee = self.market.fees[BYBIT].taker;
        match side {
            Side::Sell => {
                bybit.buy_fill(qty).is_some_and(|(_, worst)| {
                    hedge.quote >= qty * worst * (1.0 + HEDGE_SLIPPAGE) * (1.0 + hedge_fee)
                }) && okx.base >= qty
            }
            Side::Buy => {
                okx.quote >= qty * price * (1.0 + self.market.fees[OKX].maker) && hedge.base >= qty
            }
        }
    }

    /// A public OKX trade: fills the maker orders it reaches, then hedges.
    pub(super) async fn on_trade(&mut self, trade: Trade) -> Result<()> {
        if self.halted.is_some() {
            return Ok(());
        }
        let fee = self.market.fees[OKX].maker;
        let Some(maker) = &mut self.maker else {
            return Ok(());
        };
        let mut filled_sides = Vec::new();
        for side in [Side::Buy, Side::Sell] {
            let i = index(side);
            let Some(quote) = &mut maker.quotes[i] else {
                continue;
            };
            let qty = match_trade(quote, side, trade.price, trade.qty);
            if qty <= 0.0 {
                continue;
            }
            let price = quote.price;
            if quote.qty <= f64::EPSILON * qty {
                maker.quotes[i] = None;
            }
            let cash = self.inventory.wallets[OKX].book(side, qty, qty * price, fee);
            maker.unhedged[i] += qty;
            maker.unhedged_cash[i] += cash;
            self.stats.maker_fills += 1;
            self.ledger.write(json!({
                "type": "maker_fill",
                "side": side_id(side),
                "price": price,
                "qty": qty,
                "trade": [trade.price, trade.qty],
            }))?;
            filled_sides.push(side);
        }
        for side in filled_sides {
            self.hedge(side).await?;
        }
        Ok(())
    }

    /// Hedges one side's fills on Bybit; amounts below Bybit's minimum wait.
    pub(super) async fn hedge(&mut self, side: Side) -> Result<()> {
        let i = index(side);
        let Some(maker) = &self.maker else {
            return Ok(());
        };
        let m = &self.market;
        let qty = floor_to(maker.unhedged[i], m.step);
        let hedge_side = opposite(side);
        let top = {
            let book = self.books[BYBIT].borrow();
            match hedge_side {
                Side::Buy => book.asks.first().map(|l| l.0),
                Side::Sell => book.bids.first().map(|l| l.0),
            }
        };
        let Some(top) = top else {
            return Ok(());
        };
        if qty < m.min_qty || qty * top < m.min_notional {
            return Ok(());
        }
        let limit = match hedge_side {
            Side::Buy => top * (1.0 + HEDGE_SLIPPAGE),
            Side::Sell => top * (1.0 - HEDGE_SLIPPAGE),
        };
        let fill = self.ioc(BYBIT, hedge_side, qty, limit).await?;
        if fill.qty <= 0.0 {
            self.stats.hedge_misses += 1;
            return Ok(());
        }
        let fee = self.market.fees[BYBIT].taker;
        let cash = self.inventory.wallets[BYBIT].book(hedge_side, fill.qty, fill.value(), fee);
        let maker = self.maker.as_mut().expect("checked above");
        let share = (fill.qty / maker.unhedged[i]).min(1.0);
        let maker_cash = maker.unhedged_cash[i] * share;
        maker.unhedged[i] -= fill.qty;
        maker.unhedged_cash[i] -= maker_cash;
        let pnl = maker_cash + cash;
        if fill.qty < qty {
            self.stats.hedge_misses += 1;
        }
        self.stats.maker_hedges += 1;
        self.stats.maker_pnl += pnl;
        self.stats.pnl += pnl;
        self.day_pnl += pnl;
        self.day_trades += 1;
        let notional = maker_cash.abs();
        println!(
            "[{}] maker: {} {:.4} {} na OKX EEA, zabezpieczenie na Bybit EU po {:.6}: wynik {:+.4} {} ({:+.1} bp)",
            clock(),
            match side {
                Side::Buy => "kupno",
                Side::Sell => "sprzedaż",
            },
            fill.qty,
            self.market.base,
            fill.avg_price,
            pnl,
            self.market.quote,
            pnl / notional * 1e4
        );
        self.ledger.write(json!({
            "type": "maker_hedge",
            "side": side_id(side),
            "qty": fill.qty,
            "hedge_price": fill.avg_price,
            "pnl": pnl,
            "pnl_bps": pnl / notional * 1e4,
            "missed": qty - fill.qty,
            "orders": self.take_orders(),
        }))?;
        self.last_tick_ms = now_ms();
        self.check_loss_limit()
    }
}

/// Base a public trade fills on our order: through our price at once, at our price
/// only after the queue ahead.
fn match_trade(quote: &mut Resting, side: Side, price: f64, qty: f64) -> f64 {
    let tolerance = quote.price * 1e-9;
    let through = match side {
        Side::Sell => price > quote.price + tolerance,
        Side::Buy => price < quote.price - tolerance,
    };
    let reaching = if through {
        qty
    } else if (price - quote.price).abs() <= tolerance {
        let reaching = qty - quote.queue_ahead;
        quote.queue_ahead = (quote.queue_ahead - qty).max(0.0);
        reaching.max(0.0)
    } else {
        0.0
    };
    let filled = reaching.min(quote.qty);
    quote.qty -= filled;
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resting(qty: f64, queue_ahead: f64) -> Resting {
        Resting {
            price: 1.0,
            qty,
            queue_ahead,
        }
    }

    #[test]
    fn maker_order_fills_after_the_queue_or_by_a_trade_through() {
        let mut sell = resting(10.0, 5.0);
        // A buyer at our price first takes the 5 queued before us.
        assert_eq!(match_trade(&mut sell, Side::Sell, 1.0, 3.0), 0.0);
        assert_eq!(match_trade(&mut sell, Side::Sell, 1.0, 4.0), 2.0);
        // A trade through our price fills what is left.
        assert_eq!(match_trade(&mut sell, Side::Sell, 1.01, 20.0), 8.0);
        assert_eq!(sell.qty, 0.0);
        // A trade below our ask does not reach a sell.
        assert_eq!(
            match_trade(&mut resting(10.0, 0.0), Side::Sell, 0.99, 5.0),
            0.0
        );
        assert_eq!(
            match_trade(&mut resting(10.0, 0.0), Side::Buy, 0.99, 4.0),
            4.0
        );
    }
}
