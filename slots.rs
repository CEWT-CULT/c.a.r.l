use crate::error::ContractError;
use crate::phases::{
    anchor_test_race_phases, compute_pipeline_enrolling_phases, compute_test_phases,
    is_betting_open, is_crowd_commit_open, is_entry_open,
};
use crate::state::{RaceGlobal, ENROLLING_RACE, RACE_GLOBAL};
use crate::vault::require_no_native_funds;
use cosmwasm_std::{Deps, DepsMut, Env, MessageInfo, Response, StdResult, Timestamp, Uint128};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrySlot {
    Running,
    Enrolling,
}

#[derive(Clone, Debug)]
pub struct SlotContext {
    pub running: RaceGlobal,
    pub enrolling: Option<RaceGlobal>,
}

pub fn load_slots(deps: Deps) -> StdResult<SlotContext> {
    Ok(SlotContext {
        running: RACE_GLOBAL.load(deps.storage)?,
        enrolling: ENROLLING_RACE.may_load(deps.storage)?.flatten(),
    })
}

pub fn entry_slot(ctx: &SlotContext, now: Timestamp) -> Option<EntrySlot> {
    if !ctx.running.is_settled && is_entry_open(now, &ctx.running) {
        return Some(EntrySlot::Running);
    }
    if let Some(ref enrolling) = ctx.enrolling {
        if is_entry_open(now, enrolling) {
            return Some(EntrySlot::Enrolling);
        }
    }
    None
}

pub fn betting_slot(ctx: &SlotContext, now: Timestamp, test_mode: bool) -> Option<EntrySlot> {
    if let Some(ref enrolling) = ctx.enrolling {
        if is_betting_open(now, enrolling, test_mode) {
            return Some(EntrySlot::Enrolling);
        }
    }
    if !ctx.running.is_settled && is_betting_open(now, &ctx.running, test_mode) {
        return Some(EntrySlot::Running);
    }
    None
}

pub fn crowd_commit_slot(
    ctx: &SlotContext,
    now: Timestamp,
    test_mode: bool,
) -> Option<EntrySlot> {
    if !ctx.running.is_settled && is_crowd_commit_open(now, &ctx.running, test_mode) {
        return Some(EntrySlot::Running);
    }
    if let Some(ref enrolling) = ctx.enrolling {
        if is_crowd_commit_open(now, enrolling, test_mode) {
            return Some(EntrySlot::Enrolling);
        }
    }
    None
}

pub fn race_for_slot<'a>(ctx: &'a SlotContext, slot: EntrySlot) -> &'a RaceGlobal {
    match slot {
        EntrySlot::Running => &ctx.running,
        EntrySlot::Enrolling => ctx
            .enrolling
            .as_ref()
            .expect("enrolling slot without enrolling race"),
    }
}

pub fn save_slot_race(deps: &mut DepsMut, slot: EntrySlot, race: RaceGlobal) -> StdResult<()> {
    match slot {
        EntrySlot::Running => RACE_GLOBAL.save(deps.storage, &race),
        EntrySlot::Enrolling => ENROLLING_RACE.save(deps.storage, &Some(race)),
    }
}

pub fn clear_enrolling(deps: DepsMut) -> StdResult<()> {
    ENROLLING_RACE.remove(deps.storage);
    Ok(())
}

pub fn execute_open_next_race(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = crate::state::CONFIG.load(deps.storage)?;
    let running = RACE_GLOBAL.load(deps.storage)?;

    if running.is_settled {
        return Err(ContractError::WrongPhase {});
    }
    if env.block.time < running.phase_2_close {
        return Err(ContractError::WrongPhase {});
    }
    if ENROLLING_RACE.may_load(deps.storage)?.flatten().is_some() {
        return Ok(Response::new()
            .add_attribute("action", "open_next_race")
            .add_attribute("already_open", "true"));
    }

    let spawn = env.block.time;
    let schedule = if config.test_mode {
        compute_test_phases(spawn)
    } else {
        compute_pipeline_enrolling_phases(&running, spawn.seconds())
    };

    let enrolling = RaceGlobal {
        current_race_id: running.current_race_id + 1,
        total_runners: 0,
        total_entry_pool: Uint128::zero(),
        total_bet_pool: Uint128::zero(),
        phase_1_close: schedule.phase_1_close,
        phase_2_close: schedule.phase_2_close,
        phase_3_open: schedule.phase_3_open,
        phase_3_close: schedule.phase_3_close,
        crowd_commit_close: schedule.crowd_commit_close,
        crowd_reveal_close: schedule.crowd_reveal_close,
        crowd_commit_count: 0,
        is_settled: false,
        preview_step: 0,
        last_preview_crank: crate::state::zero_timestamp(),
    };
    ENROLLING_RACE.save(deps.storage, &Some(enrolling.clone()))?;

    Ok(Response::new()
        .add_attribute("action", "open_next_race")
        .add_attribute("race_id", enrolling.current_race_id.to_string())
        .add_attribute("operator", info.sender))
}

pub fn on_first_runner(race: &mut RaceGlobal, test_mode: bool, anchor: Timestamp) {
    if test_mode && race.total_runners == 1 {
        anchor_test_race_phases(race, anchor);
    }
}
