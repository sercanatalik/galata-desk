//! The Deposits and withdrawals tab: each movement of money, in words.

use rust_decimal::Decimal;

use super::{short_address, words};
use crate::venue::{LedgerDelta, LedgerRow};

const PERPS: &str = "Perpetuals";
const SPOT: &str = "Spot";
const BRIDGE: &str = "Arbitrum";

/// One row of the tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub time_ms: u64,
    pub hash: String,
    /// `Deposit`, `Withdrawal`, `Transfer`, `Send`, `Vault deposit`, …
    pub action: String,
    pub from: String,
    pub to: String,
    /// The change in the account's value; `None` when the money stayed in
    /// the account (between its own balances, or into its vault equity).
    pub change: Option<Decimal>,
    /// The fee paid; `None` when the venue reports none.
    pub fee: Option<Decimal>,
    /// For a deposit or withdrawal: what was bridged.
    pub bridged: bool,
}

/// Where a send's money sat, by the venue's dex name.
fn side_of(dex: &str) -> String {
    match dex {
        "" => PERPS.to_string(),
        "spot" => SPOT.to_string(),
        other => format!("{other} perpetuals"),
    }
}

/// One ledger row as the tab lists it. `account` is the connected address,
/// which decides the direction of a send.
pub fn transfer(row: &LedgerRow, account: &str) -> Transfer {
    let me = |a: &str| a.eq_ignore_ascii_case(account);
    let mut t = Transfer {
        time_ms: row.time,
        hash: row.hash.clone(),
        action: String::new(),
        from: "\u{2014}".into(),
        to: "\u{2014}".into(),
        change: None,
        fee: None,
        bridged: false,
    };
    // A movement between two addresses: in when the account receives.
    let between = |t: &mut Transfer, user: &str, destination: &str, here: String, usd: Decimal| {
        if me(destination) {
            t.from = short_address(user);
            t.to = here;
            t.change = Some(usd);
        } else {
            t.from = here;
            t.to = short_address(destination);
            t.change = Some(-usd);
        }
    };
    match &row.delta {
        LedgerDelta::Deposit { usdc } => {
            t.action = "Deposit".into();
            t.from = BRIDGE.into();
            t.to = PERPS.into();
            t.change = Some(*usdc);
            t.bridged = true;
        }
        LedgerDelta::Withdraw { usdc, fee } => {
            t.action = "Withdrawal".into();
            t.from = PERPS.into();
            t.to = BRIDGE.into();
            t.change = Some(-*usdc);
            t.fee = Some(*fee);
            t.bridged = true;
        }
        LedgerDelta::AccountClassTransfer { to_perp, .. } => {
            t.action = "Transfer".into();
            (t.from, t.to) = if *to_perp {
                (SPOT.into(), PERPS.into())
            } else {
                (PERPS.into(), SPOT.into())
            };
        }
        LedgerDelta::InternalTransfer {
            usdc,
            user,
            destination,
            fee,
        } => {
            t.action = "Send".into();
            between(&mut t, user, destination, PERPS.into(), *usdc);
            t.fee = Some(*fee);
        }
        LedgerDelta::SubAccountTransfer {
            usdc,
            user,
            destination,
        } => {
            t.action = "Sub-account transfer".into();
            between(&mut t, user, destination, PERPS.into(), *usdc);
        }
        LedgerDelta::Send {
            user,
            destination,
            source_dex,
            destination_dex,
            usdc_value,
            fee,
            token,
            ..
        } => {
            t.action = if token == "USDC" {
                "Send".into()
            } else {
                format!("Send {token}")
            };
            if me(user) && me(destination) {
                // Between the account's own dexes: no change in value.
                t.from = side_of(source_dex);
                t.to = side_of(destination_dex);
            } else {
                let here = if me(destination) {
                    side_of(destination_dex)
                } else {
                    side_of(source_dex)
                };
                between(&mut t, user, destination, here, *usdc_value);
            }
            t.fee = Some(*fee);
        }
        LedgerDelta::SpotTransfer {
            user,
            destination,
            token,
            usdc_value,
            fee,
            ..
        } => {
            t.action = format!("Send {token}");
            between(&mut t, user, destination, SPOT.into(), *usdc_value);
            t.fee = Some(*fee);
        }
        LedgerDelta::VaultCreate { vault, fee, .. } => {
            t.action = "Vault created".into();
            t.from = PERPS.into();
            t.to = short_address(vault);
            t.fee = Some(*fee);
        }
        LedgerDelta::VaultDeposit { vault, .. } => {
            t.action = "Vault deposit".into();
            t.from = PERPS.into();
            t.to = short_address(vault);
        }
        LedgerDelta::VaultWithdraw {
            vault,
            requested_usd,
            net_withdrawn_usd,
        } => {
            t.action = "Vault withdrawal".into();
            t.from = short_address(vault);
            t.to = PERPS.into();
            // What the leader and closing positions took on the way out.
            let cost = *requested_usd - *net_withdrawn_usd;
            t.fee = (!cost.is_zero()).then_some(cost);
        }
        LedgerDelta::VaultDistribution { vault, usdc } => {
            t.action = "Vault distribution".into();
            t.from = short_address(vault);
            t.to = PERPS.into();
            t.change = Some(*usdc);
        }
        LedgerDelta::Other { kind, .. } => {
            t.action = match kind.as_str() {
                "cStakingTransfer" => "Staking".into(),
                "spotGenesis" => "Spot genesis".into(),
                "liquidation" => "Liquidation".into(),
                other => words(other),
            };
        }
    }
    t
}

/// Every row held, newest first.
pub fn transfers(rows: &[LedgerRow], account: &str) -> Vec<Transfer> {
    let mut out: Vec<Transfer> = rows.iter().map(|r| transfer(r, account)).collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.time_ms));
    out
}

/// What the tab sums over the rows it lists.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LedgerTotals {
    pub rows: usize,
    pub deposited: Decimal,
    pub withdrawn: Decimal,
    pub fees: Decimal,
}

pub fn totals<'a>(rows: impl IntoIterator<Item = &'a Transfer>) -> LedgerTotals {
    let mut t = LedgerTotals::default();
    for r in rows {
        t.rows += 1;
        match (r.action.as_str(), r.change) {
            ("Deposit", Some(c)) => t.deposited += c,
            ("Withdrawal", Some(c)) => t.withdrawn += -c,
            _ => {}
        }
        t.fees += r.fee.unwrap_or_default();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "0x18c281d74feb0b9f73aebdac22f14dadf2843ab1";
    const THEM: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn row(delta: LedgerDelta) -> LedgerRow {
        LedgerRow {
            time: 1,
            hash: "0xabc".into(),
            delta,
        }
    }

    #[test]
    fn a_deposit_arrives_from_the_bridge_with_no_fee_reported() {
        let t = transfer(
            &row(LedgerDelta::Deposit {
                usdc: d("18706.66"),
            }),
            ME,
        );
        assert_eq!(t.action, "Deposit");
        assert_eq!((t.from.as_str(), t.to.as_str()), ("Arbitrum", "Perpetuals"));
        assert_eq!(t.change, Some(d("18706.66")));
        assert_eq!(t.fee, None);
    }

    #[test]
    fn a_withdrawal_leaves_with_its_fee() {
        let t = transfer(
            &row(LedgerDelta::Withdraw {
                usdc: d("500"),
                fee: d("1"),
            }),
            ME,
        );
        assert_eq!(t.action, "Withdrawal");
        assert_eq!(t.change, Some(d("-500")));
        assert_eq!(t.fee, Some(d("1")));
    }

    #[test]
    fn a_transfer_to_spot_does_not_change_the_account_value() {
        let t = transfer(
            &row(LedgerDelta::AccountClassTransfer {
                usdc: d("500"),
                to_perp: false,
            }),
            ME,
        );
        assert_eq!(t.action, "Transfer");
        assert_eq!((t.from.as_str(), t.to.as_str()), ("Perpetuals", "Spot"));
        assert_eq!(t.change, None);
    }

    #[test]
    fn a_send_is_in_or_out_by_who_received_it() {
        let incoming = transfer(
            &row(LedgerDelta::InternalTransfer {
                usdc: d("10"),
                user: THEM.into(),
                destination: ME.into(),
                fee: d("0"),
            }),
            ME,
        );
        assert_eq!(incoming.change, Some(d("10")));
        assert_eq!(incoming.from, "0x0104\u{2026}703a");
        assert_eq!(incoming.to, "Perpetuals");
        let outgoing = transfer(
            &row(LedgerDelta::SubAccountTransfer {
                usdc: d("1000"),
                user: ME.into(),
                destination: THEM.into(),
            }),
            ME,
        );
        assert_eq!(outgoing.change, Some(d("-1000")));
        assert_eq!(outgoing.action, "Sub-account transfer");
        assert_eq!(outgoing.to, "0x0104\u{2026}703a");
        let own_dexes = transfer(
            &row(LedgerDelta::Send {
                user: ME.into(),
                destination: ME.into(),
                source_dex: "".into(),
                destination_dex: "xyz".into(),
                token: "USDC".into(),
                amount: d("50"),
                usdc_value: d("50"),
                fee: d("0"),
            }),
            ME,
        );
        assert_eq!(own_dexes.change, None);
        assert_eq!(own_dexes.to, "xyz perpetuals");
    }

    #[test]
    fn vault_movements_stay_in_the_account_and_name_the_vault() {
        let vault = "0x67c9913781023f46ab7881da0ab2a453db56862c";
        let dep = transfer(
            &row(LedgerDelta::VaultDeposit {
                vault: vault.into(),
                usdc: d("75"),
            }),
            ME,
        );
        assert_eq!(dep.action, "Vault deposit");
        assert_eq!(dep.change, None);
        assert_eq!(dep.to, "0x67c9\u{2026}862c");
        let out = transfer(
            &row(LedgerDelta::VaultWithdraw {
                vault: vault.into(),
                requested_usd: d("75"),
                net_withdrawn_usd: d("70"),
            }),
            ME,
        );
        assert_eq!(out.fee, Some(d("5")));
        let paid = transfer(
            &row(LedgerDelta::VaultDistribution {
                vault: vault.into(),
                usdc: d("104.5"),
            }),
            ME,
        );
        assert_eq!(paid.change, Some(d("104.5")));
    }

    #[test]
    fn a_type_this_build_does_not_read_is_listed_under_its_own_name() {
        let other = |kind: &str| {
            transfer(
                &row(LedgerDelta::Other {
                    kind: kind.into(),
                    fields: serde_json::json!({}),
                }),
                ME,
            )
            .action
        };
        assert_eq!(other("cStakingTransfer"), "Staking");
        assert_eq!(other("liquidation"), "Liquidation");
        assert_eq!(other("borrowLend"), "Borrow lend");
    }

    #[test]
    fn totals_count_deposits_withdrawals_and_fees() {
        let rows = [
            transfer(
                &row(LedgerDelta::Deposit {
                    usdc: d("18706.66"),
                }),
                ME,
            ),
            transfer(&row(LedgerDelta::Deposit { usdc: d("19.80") }), ME),
            transfer(
                &row(LedgerDelta::Withdraw {
                    usdc: d("100"),
                    fee: d("1"),
                }),
                ME,
            ),
        ];
        let t = totals(&rows);
        assert_eq!(t.deposited, d("18726.46"));
        assert_eq!(t.withdrawn, d("100"));
        assert_eq!(t.fees, d("1"));
        assert_eq!(t.rows, 3);
    }
}
