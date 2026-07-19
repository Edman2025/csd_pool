use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AccountingError {
    #[error("fee bps must be <= 10000")]
    InvalidFeeBps,
    #[error("share window is empty")]
    EmptyShareWindow,
    #[error("total share difficulty is zero")]
    ZeroShareDifficulty,
}

pub type Result<T> = std::result::Result<T, AccountingError>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ShareWeight {
    pub miner: String,
    pub difficulty: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RewardAllocation {
    pub miner: String,
    pub amount_base_units: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    RewardImmature,
    RewardMature,
    RewardOrphanReversal,
    PayoutLock,
    PayoutSent,
    PayoutFailedUnlock,
    PoolFee,
    ManualAdjustment,
}

impl LedgerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LedgerKind::RewardImmature => "reward_immature",
            LedgerKind::RewardMature => "reward_mature",
            LedgerKind::RewardOrphanReversal => "reward_orphan_reversal",
            LedgerKind::PayoutLock => "payout_lock",
            LedgerKind::PayoutSent => "payout_sent",
            LedgerKind::PayoutFailedUnlock => "payout_failed_unlock",
            LedgerKind::PoolFee => "pool_fee",
            LedgerKind::ManualAdjustment => "manual_adjustment",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LedgerEntry {
    pub miner: Option<String>,
    pub amount_base_units: i128,
    pub kind: LedgerKind,
    pub ref_type: String,
    pub ref_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PplnsResult {
    pub reward_base_units: u128,
    pub fee_base_units: u128,
    pub miner_total_base_units: u128,
    pub allocations: Vec<RewardAllocation>,
}

pub fn allocate_pplns(
    reward_base_units: u128,
    fee_bps: u16,
    shares: &[ShareWeight],
) -> Result<PplnsResult> {
    if fee_bps > 10_000 {
        return Err(AccountingError::InvalidFeeBps);
    }
    if shares.is_empty() {
        return Err(AccountingError::EmptyShareWindow);
    }

    let mut weights = BTreeMap::<String, u128>::new();
    for share in shares {
        *weights.entry(share.miner.clone()).or_default() += share.difficulty as u128;
    }
    let total_weight: u128 = weights.values().sum();
    if total_weight == 0 {
        return Err(AccountingError::ZeroShareDifficulty);
    }

    let fee_base_units = reward_base_units * fee_bps as u128 / 10_000;
    let miner_total_base_units = reward_base_units.saturating_sub(fee_base_units);

    let mut allocations = Vec::with_capacity(weights.len());
    let mut allocated = 0u128;
    let last_miner = weights.keys().next_back().cloned();
    for (miner, weight) in weights {
        let mut amount = miner_total_base_units * weight / total_weight;
        if Some(&miner) == last_miner.as_ref() {
            amount = miner_total_base_units.saturating_sub(allocated);
        }
        allocated += amount;
        allocations.push(RewardAllocation {
            miner,
            amount_base_units: amount,
        });
    }

    Ok(PplnsResult {
        reward_base_units,
        fee_base_units,
        miner_total_base_units,
        allocations,
    })
}

pub fn reward_ledger_entries(block_ref: &str, result: &PplnsResult) -> Vec<LedgerEntry> {
    let mut entries = result
        .allocations
        .iter()
        .map(|allocation| LedgerEntry {
            miner: Some(allocation.miner.clone()),
            amount_base_units: allocation.amount_base_units as i128,
            kind: LedgerKind::RewardImmature,
            ref_type: "block".to_owned(),
            ref_id: block_ref.to_owned(),
        })
        .collect::<Vec<_>>();

    if result.fee_base_units > 0 {
        entries.push(LedgerEntry {
            miner: None,
            amount_base_units: result.fee_base_units as i128,
            kind: LedgerKind::PoolFee,
            ref_type: "block".to_owned(),
            ref_id: block_ref.to_owned(),
        });
    }
    entries
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MinerBalance {
    pub miner: String,
    pub address: String,
    pub confirmed_base_units: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PayoutRecipient {
    pub miner: String,
    pub address: String,
    pub amount_base_units: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PayoutSelection {
    pub recipients: Vec<PayoutRecipient>,
    pub total_base_units: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PayoutBatchDraft {
    pub batch_id: String,
    pub recipients: Vec<PayoutRecipient>,
    pub total_base_units: u128,
    pub lock_entries: Vec<LedgerEntry>,
}

pub fn select_payouts(
    balances: &[MinerBalance],
    minimum_payout_base_units: u128,
    max_recipients: usize,
) -> PayoutSelection {
    let mut recipients = balances
        .iter()
        .filter(|balance| balance.confirmed_base_units >= minimum_payout_base_units)
        .map(|balance| PayoutRecipient {
            miner: balance.miner.clone(),
            address: balance.address.clone(),
            amount_base_units: balance.confirmed_base_units,
        })
        .collect::<Vec<_>>();

    recipients.sort_by(|a, b| {
        b.amount_base_units
            .cmp(&a.amount_base_units)
            .then_with(|| a.miner.cmp(&b.miner))
    });
    recipients.truncate(max_recipients);
    let total_base_units = recipients
        .iter()
        .map(|recipient| recipient.amount_base_units)
        .sum();

    PayoutSelection {
        recipients,
        total_base_units,
    }
}

pub fn payout_batch_draft(
    batch_id: impl Into<String>,
    selection: PayoutSelection,
) -> PayoutBatchDraft {
    let batch_id = batch_id.into();
    let lock_entries = selection
        .recipients
        .iter()
        .map(|recipient| LedgerEntry {
            miner: Some(recipient.miner.clone()),
            amount_base_units: -(recipient.amount_base_units as i128),
            kind: LedgerKind::PayoutLock,
            ref_type: "payout_batch".to_owned(),
            ref_id: batch_id.clone(),
        })
        .collect();
    PayoutBatchDraft {
        batch_id,
        total_base_units: selection.total_base_units,
        recipients: selection.recipients,
        lock_entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_pplns_by_share_weight_and_fee() {
        let result = allocate_pplns(
            5_000_000_000,
            100,
            &[
                ShareWeight {
                    miner: "a".to_owned(),
                    difficulty: 1,
                },
                ShareWeight {
                    miner: "b".to_owned(),
                    difficulty: 3,
                },
            ],
        )
        .unwrap();

        assert_eq!(result.fee_base_units, 50_000_000);
        assert_eq!(result.miner_total_base_units, 4_950_000_000);
        assert_eq!(result.allocations[0].amount_base_units, 1_237_500_000);
        assert_eq!(result.allocations[1].amount_base_units, 3_712_500_000);
        assert_eq!(
            result
                .allocations
                .iter()
                .map(|allocation| allocation.amount_base_units)
                .sum::<u128>(),
            result.miner_total_base_units
        );
    }

    #[test]
    fn aggregates_multiple_shares_per_miner() {
        let result = allocate_pplns(
            100,
            0,
            &[
                ShareWeight {
                    miner: "a".to_owned(),
                    difficulty: 1,
                },
                ShareWeight {
                    miner: "a".to_owned(),
                    difficulty: 1,
                },
                ShareWeight {
                    miner: "b".to_owned(),
                    difficulty: 2,
                },
            ],
        )
        .unwrap();
        assert_eq!(result.allocations[0].amount_base_units, 50);
        assert_eq!(result.allocations[1].amount_base_units, 50);
    }

    #[test]
    fn rejects_empty_or_zero_share_window() {
        assert!(matches!(
            allocate_pplns(100, 0, &[]),
            Err(AccountingError::EmptyShareWindow)
        ));
        assert!(matches!(
            allocate_pplns(
                100,
                0,
                &[ShareWeight {
                    miner: "a".to_owned(),
                    difficulty: 0
                }]
            ),
            Err(AccountingError::ZeroShareDifficulty)
        ));
    }

    #[test]
    fn selects_payouts_by_threshold_and_limit() {
        let selection = select_payouts(
            &[
                MinerBalance {
                    miner: "a".to_owned(),
                    address: "addr-a".to_owned(),
                    confirmed_base_units: 50,
                },
                MinerBalance {
                    miner: "b".to_owned(),
                    address: "addr-b".to_owned(),
                    confirmed_base_units: 200,
                },
                MinerBalance {
                    miner: "c".to_owned(),
                    address: "addr-c".to_owned(),
                    confirmed_base_units: 150,
                },
            ],
            100,
            1,
        );
        assert_eq!(selection.recipients.len(), 1);
        assert_eq!(selection.recipients[0].miner, "b");
        assert_eq!(selection.total_base_units, 200);
    }

    #[test]
    fn builds_reward_ledger_entries_with_pool_fee() {
        let result = allocate_pplns(
            1000,
            100,
            &[ShareWeight {
                miner: "a".to_owned(),
                difficulty: 1,
            }],
        )
        .unwrap();
        let entries = reward_ledger_entries("block-1", &result);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, LedgerKind::RewardImmature);
        assert_eq!(entries[0].amount_base_units, 990);
        assert_eq!(entries[1].kind, LedgerKind::PoolFee);
        assert_eq!(entries[1].amount_base_units, 10);
    }

    #[test]
    fn ledger_kind_serializes_as_database_value() {
        let entry = LedgerEntry {
            miner: Some("a".to_owned()),
            amount_base_units: 990,
            kind: LedgerKind::RewardImmature,
            ref_type: "block".to_owned(),
            ref_id: "block-1".to_owned(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"kind\":\"reward_immature\""));
        let decoded: LedgerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, LedgerKind::RewardImmature);

        assert_eq!(
            serde_json::to_string(&LedgerKind::PoolFee).unwrap(),
            "\"pool_fee\""
        );
        assert_eq!(
            serde_json::to_string(&LedgerKind::PayoutLock).unwrap(),
            "\"payout_lock\""
        );
    }

    #[test]
    fn builds_payout_batch_lock_entries() {
        let selection = PayoutSelection {
            recipients: vec![PayoutRecipient {
                miner: "a".to_owned(),
                address: "addr-a".to_owned(),
                amount_base_units: 200,
            }],
            total_base_units: 200,
        };
        let draft = payout_batch_draft("batch-1", selection);
        assert_eq!(draft.total_base_units, 200);
        assert_eq!(draft.lock_entries.len(), 1);
        assert_eq!(draft.lock_entries[0].kind, LedgerKind::PayoutLock);
        assert_eq!(draft.lock_entries[0].amount_base_units, -200);
    }
}
