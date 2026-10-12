//! Rebalancing by transfer or trade.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::json;

use super::{Engine, clock};

/// At most one rebalancing trade this often, so one imbalance isn't traded twice.
const TRADE_REBALANCE_COOLDOWN: Duration = Duration::from_secs(60);

impl Engine {
    /// When one direction lacks money and the other has room for two trades, trades in
    /// reverse if that costs at most `--rebalance-max-cost-bps`.
    pub(super) async fn rebalance_by_trade(&mut self) -> Result<()> {
        if self
            .last_trade_rebalance
            .is_some_and(|t| t.elapsed() < TRADE_REBALANCE_COOLDOWN)
        {
            return Ok(());
        }
        let Some(mid) = self.mid() else {
            return Ok(());
        };
        let w = &self.inventory.wallets;
        let room = |d: usize| w[d].quote.min(w[1 - d].base * mid);
        let Some((short, amount)) =
            trade_rebalance_need([room(0), room(1)], self.size, self.min_trade())
        else {
            return Ok(());
        };
        // Reverse: buy where the coin is missing, sell where it piles up.
        let reverse = 1 - short;
        let Some(plan) = self.plan_sized(reverse, amount) else {
            return Ok(());
        };
        if plan.expected_bps < -self.rebalance_max_cost_bps || !self.has_funds(reverse, &plan) {
            return Ok(());
        }
        self.last_trade_rebalance = Some(Instant::now());
        self.trade(reverse, plan, true).await
    }

    /// Dry run: simulated transfers when a wallet runs low.
    pub(super) fn rebalance(&mut self) -> Result<()> {
        self.inventory.settle();
        if self.trade_rebalance {
            return Ok(());
        }
        let Some(mid) = self.mid() else {
            return Ok(());
        };
        let m = &self.market;
        let Some(r) = self
            .inventory
            .rebalance(mid, m.base_fee, m.quote_fee, self.transfer)
        else {
            return Ok(());
        };
        self.stats.rebalances += 1;
        self.stats.rebalance_cost += r.cost;
        self.stats.pnl -= r.cost;
        self.day_pnl -= r.cost;
        let line = format!(
            "rebalans: {:.4} {} i {:.2} {} w drodze ({} min), koszt {:.4} {}",
            r.base,
            m.base,
            r.quote,
            m.quote,
            self.transfer.as_secs() / 60,
            r.cost,
            m.quote
        );
        println!("[{}] {line}", clock());
        self.tell(line);
        self.ledger.write(json!({
            "type": "rebalance",
            "base": r.base,
            "quote": r.quote,
            "cost": r.cost,
        }))?;
        self.check_loss_limit()
    }
}

/// Direction short of money while the other has room for two trades, and how much to
/// move: half the difference, at most two trades.
fn trade_rebalance_need(room: [f64; 2], size: f64, min_trade: f64) -> Option<(usize, f64)> {
    let short = (0..2).find(|&d| room[d] < size && room[1 - d] >= 2.0 * size)?;
    let amount = ((room[1 - short] - room[short]) / 2.0).min(2.0 * size);
    (amount >= min_trade).then_some((short, amount))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebalances_only_when_one_side_is_short_and_the_other_has_room() {
        assert_eq!(
            trade_rebalance_need([8.0, 70.0], 20.0, 5.0),
            Some((0, 31.0))
        );
        assert_eq!(
            trade_rebalance_need([0.0, 100.0], 20.0, 5.0),
            Some((0, 40.0))
        );
        assert_eq!(trade_rebalance_need([25.0, 70.0], 20.0, 5.0), None);
        assert_eq!(trade_rebalance_need([8.0, 30.0], 20.0, 5.0), None);
    }
}
