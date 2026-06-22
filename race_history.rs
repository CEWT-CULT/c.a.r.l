use crate::error::ContractError;
use crate::state::{
    RaceGlobal, RaceHistoryEntry, RACE_ENTRIES, RACE_GLOBAL, RACE_HISTORY, RACE_RESULTS,
    RACE_SIDE_BET_SETTLEMENT, SIDE_BETS,
};
use crate::vault::require_no_native_funds;
use cosmwasm_std::{Addr, Deps, DepsMut, MessageInfo, Order, Response, StdResult, Timestamp};
use cw_storage_plus::Bound;

pub const RACE_RETENTION: u64 = 365;
pub const DEFAULT_HISTORY_LIMIT: u32 = 20;
pub const MAX_HISTORY_LIMIT: u32 = 50;
pub const DEFAULT_PRUNE_LIMIT: u32 = 50;
pub const MAX_PRUNE_LIMIT: u32 = 100;

/// Sentinel rank written on admin rain-out so `claim_racer_nft` can unlock escrow.
pub const RAIN_OUT_RANK: u32 = 0;

/// Persist a settled race snapshot for paginated history queries.
pub fn archive_settled_race(
    deps: DepsMut,
    race_id: u64,
    race: &RaceGlobal,
    winner: Option<Addr>,
    settled_at: Timestamp,
    rained_out: bool,
) -> Result<(), ContractError> {
    let record = RaceHistoryEntry {
        race_id,
        total_runners: race.total_runners,
        total_entry_pool: race.total_entry_pool,
        total_bet_pool: race.total_bet_pool,
        settled_at,
        winner,
        phase_1_close: race.phase_1_close,
        phase_2_close: race.phase_2_close,
        phase_3_open: race.phase_3_open,
        rained_out,
    };
    RACE_HISTORY.save(deps.storage, race_id, &record)?;
    Ok(())
}

fn race_prune_cutoff(current_race_id: u64) -> u64 {
    current_race_id.saturating_sub(RACE_RETENTION)
}

fn race_fully_claimed(deps: Deps, race_id: u64) -> StdResult<bool> {
    let mut had_claimables = false;

    for item in RACE_ENTRIES
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
    {
        had_claimables = true;
        let (_, entry) = item?;
        if !entry.nft_claimed {
            return Ok(false);
        }
    }
    for item in SIDE_BETS
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
    {
        had_claimables = true;
        let (_, bet) = item?;
        if !bet.claimed {
            return Ok(false);
        }
    }

    Ok(had_claimables)
}

fn is_race_prunable(deps: Deps, current_race_id: u64, race_id: u64) -> StdResult<bool> {
    if race_id == 0 || race_id >= current_race_id {
        return Ok(false);
    }
    if race_fully_claimed(deps, race_id)? {
        return Ok(true);
    }
    Ok(race_id <= race_prune_cutoff(current_race_id))
}

fn race_has_leftover_data(deps: Deps, race_id: u64) -> bool {
    RACE_ENTRIES
        .prefix(race_id)
        .keys(deps.storage, None, None, Order::Ascending)
        .next()
        .is_some()
        || SIDE_BETS
            .prefix(race_id)
            .keys(deps.storage, None, None, Order::Ascending)
            .next()
            .is_some()
}

/// Permissionless batched purge — call repeatedly until `prune_complete` is true.
pub fn execute_prune_history(
    deps: DepsMut,
    info: MessageInfo,
    race_id: u64,
    limit: Option<u32>,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;

    let current = RACE_GLOBAL.load(deps.storage)?.current_race_id;
    if !is_race_prunable(deps.as_ref(), current, race_id)? {
        return Err(ContractError::RaceWithinRetention {});
    }

    let batch_limit = limit
        .unwrap_or(DEFAULT_PRUNE_LIMIT)
        .clamp(1, MAX_PRUNE_LIMIT) as usize;

    let mut removed = 0usize;

    let players: Vec<Addr> = RACE_ENTRIES
        .prefix(race_id)
        .keys(deps.storage, None, None, Order::Ascending)
        .take(batch_limit)
        .filter_map(|r| r.ok())
        .collect();
    for player in players {
        RACE_ENTRIES.remove(deps.storage, (race_id, player.clone()));
        RACE_RESULTS.remove(deps.storage, (race_id, player));
        removed += 1;
    }

    if removed < batch_limit {
        let bettors: Vec<Addr> = SIDE_BETS
            .prefix(race_id)
            .keys(deps.storage, None, None, Order::Ascending)
            .take(batch_limit - removed)
            .filter_map(|r| r.ok())
            .collect();
        for bettor in bettors {
            SIDE_BETS.remove(deps.storage, (race_id, bettor));
            removed += 1;
        }
    }

    let prune_complete = !race_has_leftover_data(deps.as_ref(), race_id);
    if prune_complete {
        RACE_HISTORY.remove(deps.storage, race_id);
        RACE_SIDE_BET_SETTLEMENT.remove(deps.storage, race_id);
    }

    Ok(Response::new()
        .add_attribute("action", "prune_history")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("removed", removed.to_string())
        .add_attribute("prune_complete", prune_complete.to_string()))
}

pub fn query_race_history_paginated(
    deps: Deps,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<(Vec<RaceHistoryEntry>, Option<u64>)> {
    let take = limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .min(MAX_HISTORY_LIMIT) as usize;

    let end = start_after.map(Bound::exclusive);
    let collected: Vec<RaceHistoryEntry> = RACE_HISTORY
        .range(deps.storage, None, end, Order::Descending)
        .take(take + 1)
        .filter_map(|r| r.ok().map(|(_, v)| v))
        .collect();

    if collected.len() > take {
        let next = collected[take - 1].race_id;
        Ok((collected[..take].to_vec(), Some(next)))
    } else {
        Ok((collected, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::mock_dependencies;
    use cosmwasm_std::{Binary, MessageInfo, Timestamp, Uint128};
    use crate::state::{BetType, RaceEntry, RaceGlobal, RaceResult, SideBet, Species};

    fn sample_record(id: u64) -> RaceHistoryEntry {
        RaceHistoryEntry {
            race_id: id,
            total_runners: 3,
            total_entry_pool: Uint128::new(3_000_000),
            total_bet_pool: Uint128::new(500_000),
            settled_at: Timestamp::from_seconds(id * 1000),
            winner: None,
            phase_1_close: Timestamp::from_seconds(0),
            phase_2_close: Timestamp::from_seconds(0),
            phase_3_open: Timestamp::from_seconds(0),
            rained_out: false,
        }
    }

    #[test]
    fn history_query_pages_newest_first() {
        let mut deps = mock_dependencies();
        for id in 1..=5u64 {
            RACE_HISTORY
                .save(deps.as_mut().storage, id, &sample_record(id))
                .unwrap();
        }

        let (page1, next1) =
            query_race_history_paginated(deps.as_ref(), None, Some(2)).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].race_id, 5);
        assert_eq!(page1[1].race_id, 4);
        assert_eq!(next1, Some(4));

        let (page2, next2) =
            query_race_history_paginated(deps.as_ref(), next1, Some(2)).unwrap();
        assert_eq!(page2[0].race_id, 3);
        assert_eq!(page2[1].race_id, 2);
        assert_eq!(next2, Some(2));

        let (page3, next3) =
            query_race_history_paginated(deps.as_ref(), next2, Some(2)).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].race_id, 1);
        assert_eq!(next3, None);
    }

    #[test]
    fn prune_history_rejects_races_within_retention() {
        let mut deps = mock_dependencies();
        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 100,
                    total_runners: 0,
                    total_entry_pool: Uint128::zero(),
                    total_bet_pool: Uint128::zero(),
                    phase_1_close: Timestamp::from_seconds(0),
                    phase_2_close: Timestamp::from_seconds(0),
                    phase_3_open: Timestamp::from_seconds(0),
                    phase_3_close: Timestamp::from_seconds(0),
                    crowd_commit_close: Timestamp::from_seconds(0),
                    crowd_reveal_close: Timestamp::from_seconds(0),
                    crowd_commit_count: 0,
                    is_settled: false,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        let err = execute_prune_history(
            deps.as_mut(),
            MessageInfo {
                sender: Addr::unchecked("anyone"),
                funds: vec![],
            },
            50,
            Some(50),
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::RaceWithinRetention {}));
    }

    #[test]
    fn prune_history_deletes_in_batches() {
        let mut deps = mock_dependencies();
        let race_id = 1u64;
        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: RACE_RETENTION + 2,
                    total_runners: 0,
                    total_entry_pool: Uint128::zero(),
                    total_bet_pool: Uint128::zero(),
                    phase_1_close: Timestamp::from_seconds(0),
                    phase_2_close: Timestamp::from_seconds(0),
                    phase_3_open: Timestamp::from_seconds(0),
                    phase_3_close: Timestamp::from_seconds(0),
                    crowd_commit_close: Timestamp::from_seconds(0),
                    crowd_reveal_close: Timestamp::from_seconds(0),
                    crowd_commit_count: 0,
                    is_settled: true,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();
        RACE_HISTORY
            .save(deps.as_mut().storage, race_id, &sample_record(race_id))
            .unwrap();

        for i in 0..5u64 {
            let player = Addr::unchecked(format!("runner{i}"));
            RACE_ENTRIES
                .save(
                    deps.as_mut().storage,
                    (race_id, player.clone()),
                    &RaceEntry {
                        player: player.clone(),
                        nft_contract: Addr::unchecked("nft"),
                        nft_id: i.to_string(),
                        species: Species::Chicken,
                        commitment: Binary::default(),
                        revealed_action: None,
                        revealed_salt: None,
                        final_rank: Some(1),
                        nft_claimed: false,
                        committed_at: crate::state::zero_timestamp(),
                    },
                )
                .unwrap();
            RACE_RESULTS
                .save(
                    deps.as_mut().storage,
                    (race_id, player),
                    &RaceResult {
                        tick_distances: [10; 5],
                        total_distance: 50,
                    },
                )
                .unwrap();
        }

        for i in 0..3u64 {
            let bettor = Addr::unchecked(format!("bettor{i}"));
            SIDE_BETS
                .save(
                    deps.as_mut().storage,
                    (race_id, bettor.clone()),
                    &SideBet {
                        bettor,
                        bet_type: BetType::ChickenVictory,
                        amount: Uint128::new(1_000),
                        pick: None,
                        claimed: true,
                    },
                )
                .unwrap();
        }

        let info = MessageInfo {
            sender: Addr::unchecked("cranker"),
            funds: vec![],
        };

        let r1 = execute_prune_history(deps.as_mut(), info.clone(), race_id, Some(2)).unwrap();
        assert_eq!(
            r1.attributes.iter().find(|a| a.key == "removed").unwrap().value,
            "2"
        );
        assert_eq!(
            r1.attributes
                .iter()
                .find(|a| a.key == "prune_complete")
                .unwrap()
                .value,
            "false"
        );
        assert!(RACE_HISTORY.has(deps.as_ref().storage, race_id));

        let r2 = execute_prune_history(deps.as_mut(), info.clone(), race_id, Some(100)).unwrap();
        assert_eq!(
            r2.attributes
                .iter()
                .find(|a| a.key == "prune_complete")
                .unwrap()
                .value,
            "true"
        );
        assert!(!RACE_HISTORY.has(deps.as_ref().storage, race_id));
        assert!(!RACE_ENTRIES.has(deps.as_ref().storage, (race_id, Addr::unchecked("runner0"))));
        assert!(!SIDE_BETS.has(deps.as_ref().storage, (race_id, Addr::unchecked("bettor0"))));
    }

    #[test]
    fn prune_history_allows_early_when_fully_claimed() {
        let mut deps = mock_dependencies();
        let race_id = 49u64;
        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 50,
                    total_runners: 0,
                    total_entry_pool: Uint128::zero(),
                    total_bet_pool: Uint128::zero(),
                    phase_1_close: Timestamp::from_seconds(0),
                    phase_2_close: Timestamp::from_seconds(0),
                    phase_3_open: Timestamp::from_seconds(0),
                    phase_3_close: Timestamp::from_seconds(0),
                    crowd_commit_close: Timestamp::from_seconds(0),
                    crowd_reveal_close: Timestamp::from_seconds(0),
                    crowd_commit_count: 0,
                    is_settled: true,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();
        RACE_HISTORY
            .save(deps.as_mut().storage, race_id, &sample_record(race_id))
            .unwrap();

        let player = Addr::unchecked("runner");
        RACE_ENTRIES
            .save(
                deps.as_mut().storage,
                (race_id, player.clone()),
                &RaceEntry {
                    player: player.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".into(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: Some(1),
                    nft_claimed: false,
                    committed_at: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        let info = MessageInfo {
            sender: Addr::unchecked("cranker"),
            funds: vec![],
        };

        let err = execute_prune_history(deps.as_mut(), info.clone(), race_id, Some(50))
            .unwrap_err();
        assert!(matches!(err, ContractError::RaceWithinRetention {}));

        RACE_ENTRIES
            .save(
                deps.as_mut().storage,
                (race_id, player),
                &RaceEntry {
                    player: Addr::unchecked("runner"),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".into(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: Some(1),
                    nft_claimed: true,
                    committed_at: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        let result = execute_prune_history(deps.as_mut(), info, race_id, Some(50)).unwrap();
        assert_eq!(
            result
                .attributes
                .iter()
                .find(|a| a.key == "prune_complete")
                .unwrap()
                .value,
            "true"
        );
        assert!(!RACE_HISTORY.has(deps.as_ref().storage, race_id));
    }
}
