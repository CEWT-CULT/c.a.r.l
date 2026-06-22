use crate::error::ContractError;
use crate::msg::PreviewRunner;
use crate::phases::{is_race_preview_open, TEST_PREVIEW_LIVE_SECS, TICKS};
use crate::settlement::{derive_master_seed, run_physics};
use crate::state::{
    default_user_profile, CROWD_ENTROPY, RACE_ENTRIES, RACE_GLOBAL, RACE_PREVIEW_SIM,
    CONFIG, USER_PROFILES,
};
use crate::vault::require_no_native_funds;
use cosmwasm_std::{Addr, Deps, DepsMut, Env, MessageInfo, Order, Response, StdResult};

/// Total preview substeps (5 ticks × 5 substeps — one public crank per minute).
pub const PREVIEW_MAX_STEPS: u8 = 25;
pub const PREVIEW_CRANK_INTERVAL_SECS: u64 = 60;
/// Live cranking only reveals up to 85% of the race; settlement unlocks the finale.
pub const PREVIEW_PROGRESS_CAP_BPS: u128 = 8500;
/// Substeps per physics tick — `PREVIEW_MAX_STEPS / TICKS`.
const SUBSTEPS_PER_TICK: u32 = 5;

/// Cranks allowed in the live window (test: 5 min → 5 cranks; prod: 25).
pub fn preview_window_crumbs(test_mode: bool) -> u32 {
    if test_mode {
        (TEST_PREVIEW_LIVE_SECS / PREVIEW_CRANK_INTERVAL_SECS) as u32
    } else {
        PREVIEW_MAX_STEPS as u32
    }
}

pub fn preview_crank_limit(test_mode: bool) -> u8 {
    preview_window_crumbs(test_mode).min(PREVIEW_MAX_STEPS as u32) as u8
}

/// Cumulative distance after `substeps` physics substeps (monotonic 0 → full race).
pub fn cumulative_at_substep(tick_distances: &[u128; TICKS], substeps: u32) -> u128 {
    if substeps == 0 {
        return 0;
    }
    let max = PREVIEW_MAX_STEPS as u32;
    let substeps = substeps.min(max);
    let per_tick = SUBSTEPS_PER_TICK;
    let full_ticks = (substeps / per_tick).min(TICKS as u32) as usize;
    let partial = substeps % per_tick;

    let mut sum: u128 = tick_distances[..full_ticks].iter().sum();
    if full_ticks < TICKS && partial > 0 {
        let idx = full_ticks;
        sum += tick_distances[idx] * (partial as u128) / (per_tick as u128);
    }
    sum
}

/// On-chain preview positions: cranks map to 0–85%; settlement query returns 100%.
pub fn cumulative_for_preview(
    tick_distances: &[u128; TICKS],
    preview_step: u8,
    test_mode: bool,
    settled: bool,
) -> u128 {
    if settled {
        return cumulative_at_substep(tick_distances, PREVIEW_MAX_STEPS as u32);
    }
    if preview_step == 0 {
        return 0;
    }
    let window = preview_window_crumbs(test_mode);
    if window == 0 {
        return 0;
    }
    let step = preview_step.min(PREVIEW_MAX_STEPS);
    let scaled = (step as u32 * PREVIEW_MAX_STEPS as u32) / window;
    let raw = cumulative_at_substep(tick_distances, scaled.min(PREVIEW_MAX_STEPS as u32));
    raw * PREVIEW_PROGRESS_CAP_BPS / 10_000
}

pub fn clear_race_preview(deps: DepsMut, race_id: u64) -> StdResult<()> {
    let addrs: Vec<Addr> = RACE_PREVIEW_SIM
        .prefix(race_id)
        .keys(deps.storage, None, None, Order::Ascending)
        .collect::<StdResult<Vec<_>>>()?;
    for addr in addrs {
        RACE_PREVIEW_SIM.remove(deps.storage, (race_id, addr));
    }
    Ok(())
}

fn load_entries_with_profiles(
    deps: Deps,
    race_id: u64,
) -> StdResult<Vec<(Addr, crate::state::RaceEntry, crate::state::UserProfile)>> {
    let prefix = RACE_ENTRIES.prefix(race_id);
    prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(addr, entry)| {
            let profile = USER_PROFILES
                .may_load(deps.storage, addr.clone())?
                .unwrap_or_else(default_user_profile);
            Ok((addr, entry, profile))
        })
        .collect()
}

fn ensure_preview_sim_cached(deps: DepsMut, race_id: u64) -> Result<(), ContractError> {
    let has_any = RACE_PREVIEW_SIM
        .prefix(race_id)
        .keys(deps.storage, None, None, Order::Ascending)
        .next()
        .transpose()?
        .is_some();
    if has_any {
        return Ok(());
    }

    let entries_with_profiles = load_entries_with_profiles(deps.as_ref(), race_id)?;
    if entries_with_profiles.is_empty() {
        return Ok(());
    }

    let entries: Vec<(Addr, crate::state::RaceEntry)> = entries_with_profiles
        .iter()
        .map(|(addr, entry, _)| (addr.clone(), entry.clone()))
        .collect();

    let crowd: Vec<(Addr, crate::state::CrowdEntropy)> = CROWD_ENTROPY
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();

    let master_seed = derive_master_seed(race_id, &entries, &crowd);
    let physics_results = run_physics(&entries_with_profiles, &master_seed);

    for (addr, result) in physics_results {
        RACE_PREVIEW_SIM.save(deps.storage, (race_id, addr), &result)?;
    }
    Ok(())
}

pub fn execute_crank_race_preview(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;

    let mut race = RACE_GLOBAL.load(deps.storage)?;
    if race.is_settled {
        return Err(ContractError::AlreadySettled {});
    }
    if !is_race_preview_open(env.block.time, &race) {
        return Err(ContractError::RacePreviewClosed {});
    }
    let config = CONFIG.load(deps.storage)?;
    let crank_limit = preview_crank_limit(config.test_mode);
    if race.preview_step >= crank_limit {
        return Err(ContractError::PreviewComplete {});
    }

    if race.preview_step > 0 {
        let elapsed = env
            .block
            .time
            .seconds()
            .saturating_sub(race.last_preview_crank.seconds());
        if elapsed < PREVIEW_CRANK_INTERVAL_SECS {
            return Err(ContractError::PreviewCrankTooSoon {});
        }
    }

    let race_id = race.current_race_id;
    ensure_preview_sim_cached(deps.branch(), race_id)?;

    race.preview_step = race.preview_step.saturating_add(1);
    race.last_preview_crank = env.block.time;
    RACE_GLOBAL.save(deps.storage, &race)?;

    Ok(Response::new()
        .add_attribute("action", "crank_race_preview")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("preview_step", race.preview_step.to_string())
        .add_attribute("preview_max", crank_limit.to_string())
        .add_attribute("operator", info.sender))
}

pub fn query_race_preview(deps: Deps, race_id: u64) -> StdResult<Vec<PreviewRunner>> {
    let race = RACE_GLOBAL.load(deps.storage)?;
    let config = CONFIG.load(deps.storage)?;
    let settled = race.current_race_id == race_id && race.is_settled;
    let step = if race.current_race_id == race_id && !race.is_settled {
        race.preview_step
    } else if settled {
        PREVIEW_MAX_STEPS
    } else {
        0
    };

    let prefix = RACE_ENTRIES.prefix(race_id);
    let mut runners: Vec<PreviewRunner> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(player, entry)| {
            let cumulative = if entry.revealed_action.is_none() {
                0
            } else {
                RACE_PREVIEW_SIM
                    .may_load(deps.storage, (race_id, player.clone()))?
                    .map(|sim| {
                        cumulative_for_preview(
                            &sim.tick_distances,
                            step,
                            config.test_mode,
                            settled,
                        )
                    })
                    .unwrap_or(0)
            };
            Ok(PreviewRunner {
                player,
                species: entry.species,
                nft_contract: entry.nft_contract,
                nft_id: entry.nft_id,
                cumulative,
                preview_step: step,
            })
        })
        .collect::<StdResult<Vec<_>>>()?;

    runners.sort_by(|a, b| a.player.cmp(&b.player));
    Ok(runners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_at_substep_is_monotonic_and_zero_at_start() {
        let ticks = [10u128, 20, 30, 40, 50];
        let mut prev = 0u128;
        for s in 0..=PREVIEW_MAX_STEPS {
            let c = cumulative_at_substep(&ticks, s as u32);
            assert!(c >= prev);
            prev = c;
        }
        assert_eq!(cumulative_at_substep(&ticks, 0), 0);
        assert_eq!(cumulative_at_substep(&ticks, PREVIEW_MAX_STEPS as u32), 150);
    }

    #[test]
    fn preview_caps_at_85_percent_until_settled() {
        let ticks = [10u128, 20, 30, 40, 50];
        let capped = cumulative_for_preview(&ticks, PREVIEW_MAX_STEPS, false, false);
        assert_eq!(capped, 150 * PREVIEW_PROGRESS_CAP_BPS / 10_000);
        let full = cumulative_for_preview(&ticks, PREVIEW_MAX_STEPS, false, true);
        assert_eq!(full, 150);
    }

    #[test]
    fn test_mode_five_crumbs_reach_preview_cap() {
        let ticks = [10u128, 20, 30, 40, 50];
        let limit = preview_crank_limit(true);
        assert_eq!(limit, 5);
        let at_cap = cumulative_for_preview(&ticks, limit, true, false);
        assert_eq!(at_cap, 150 * PREVIEW_PROGRESS_CAP_BPS / 10_000);
    }
}
