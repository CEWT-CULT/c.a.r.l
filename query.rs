use crate::msg::{CrowdEntropyDeskResponse, CrowdEntropyResponse, RaceHistoryResponse, RosterEntry, SideBetDeskResponse, SideBetEntry, TelemetryRunner, UserResponse};
use crate::phases::{current_phase, MAX_CROWD_ENTROPY};
use crate::race_history::query_race_history_paginated;
use crate::settlement::{distinct_bet_types, is_one_sided_market};
use crate::state::{
    default_user_profile, default_users, CONFIG, CROWD_ENTROPY, RACE_ENTRIES, RACE_GLOBAL,
    RACE_RESULTS, RACE_SIDE_BET_SETTLEMENT, SIDE_BETS, USER_PROFILES, USERS,
};
use cosmwasm_std::{to_json_binary, Addr, Binary, Deps, Env, Order, StdResult, Uint128};

pub fn query_config(deps: Deps) -> StdResult<Binary> {
    let config = CONFIG.load(deps.storage)?;
    to_json_binary(&config)
}

pub fn query_race_global(deps: Deps) -> StdResult<Binary> {
    let race = RACE_GLOBAL.load(deps.storage)?;
    to_json_binary(&race)
}

pub fn query_user(deps: Deps, addr: Addr) -> StdResult<Binary> {
    let user = USERS
        .may_load(deps.storage, addr.clone())?
        .unwrap_or_else(default_users);
    let profile = USER_PROFILES
        .may_load(deps.storage, addr)?
        .unwrap_or_else(default_user_profile);
    to_json_binary(&UserResponse {
        deposits: user.deposits,
        last_action: user.last_action,
        profile,
    })
}

pub fn query_race_entry(deps: Deps, race_id: u64, addr: Addr) -> StdResult<Binary> {
    let entry = RACE_ENTRIES.load(deps.storage, (race_id, addr))?;
    to_json_binary(&entry)
}

pub fn query_race_roster(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let prefix = RACE_ENTRIES.prefix(race_id);
    let roster: Vec<RosterEntry> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(player, e)| RosterEntry {
            player,
            nft_contract: e.nft_contract,
            nft_id: e.nft_id,
            species: e.species,
            revealed_action: e.revealed_action,
            final_rank: e.final_rank,
            nft_claimed: e.nft_claimed,
        })
        .collect();
    to_json_binary(&roster)
}

pub fn query_side_bet(deps: Deps, race_id: u64, addr: Addr) -> StdResult<Binary> {
    let bet = SIDE_BETS.load(deps.storage, (race_id, addr))?;
    to_json_binary(&bet)
}

pub fn query_side_bet_desk(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let prefix = SIDE_BETS.prefix(race_id);
    let bets: Vec<(Addr, crate::state::SideBet)> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();

    let total_pool: Uint128 = bets
        .iter()
        .map(|(_, b)| b.amount)
        .fold(Uint128::zero(), |acc, a| acc.checked_add(a).unwrap_or(acc));

    let entries: Vec<SideBetEntry> = bets
        .iter()
        .map(|(bettor, bet)| SideBetEntry {
            bettor: bettor.clone(),
            bet_type: bet.bet_type.clone(),
            amount: bet.amount,
            pick: bet.pick.clone(),
        })
        .collect();

    let distinct = distinct_bet_types(&bets) as u32;
    let one_sided = is_one_sided_market(&bets);

    to_json_binary(&SideBetDeskResponse {
        bets: entries,
        distinct_bet_types: distinct,
        one_sided,
        total_pool,
    })
}

pub fn query_race_history(
    deps: Deps,
    start_after: Option<u64>,
    limit: Option<u32>,
) -> StdResult<Binary> {
    let (races, next) = query_race_history_paginated(deps, start_after, limit)?;
    to_json_binary(&RaceHistoryResponse {
        races,
        next,
        retention_limit: crate::race_history::RACE_RETENTION,
    })
}

pub fn query_race_telemetry(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let prefix = RACE_ENTRIES.prefix(race_id);
    let telemetry: Vec<TelemetryRunner> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(player, e)| {
            let tick_distances = RACE_RESULTS
                .may_load(deps.storage, (race_id, player.clone()))?
                .map(|r| r.tick_distances)
                .unwrap_or([0u128; 5]);
            Ok(TelemetryRunner {
                player,
                species: e.species,
                tick_distances,
                final_rank: e.final_rank,
            })
        })
        .collect::<StdResult<Vec<_>>>()?;
    to_json_binary(&telemetry)
}

pub fn query_race_preview(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let runners = crate::preview::query_race_preview(deps, race_id)?;
    to_json_binary(&runners)
}

pub fn query_current_phase(deps: Deps, env: Env) -> StdResult<Binary> {
    let race = RACE_GLOBAL.load(deps.storage)?;
    let phase = current_phase(env.block.time, &race);
    to_json_binary(&phase)
}

pub fn query_side_bet_settlement(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let settlement = RACE_SIDE_BET_SETTLEMENT.load(deps.storage, race_id)?;
    to_json_binary(&settlement)
}

pub fn query_crowd_entropy(deps: Deps, race_id: u64, addr: Addr) -> StdResult<Binary> {
    let row = CROWD_ENTROPY.load(deps.storage, (race_id, addr.clone()))?;
    to_json_binary(&CrowdEntropyResponse {
        bettor: addr,
        commitment: row.commitment,
        revealed: row.revealed_salt.is_some(),
    })
}

pub fn query_crowd_entropy_desk(deps: Deps, race_id: u64) -> StdResult<Binary> {
    let prefix = CROWD_ENTROPY.prefix(race_id);
    let rows: Vec<_> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(_, row)| row)
        .collect();
    let reveals = rows.iter().filter(|r| r.revealed_salt.is_some()).count() as u32;
    to_json_binary(&CrowdEntropyDeskResponse {
        commits: rows.len() as u32,
        reveals,
        max_commits: MAX_CROWD_ENTROPY,
    })
}
