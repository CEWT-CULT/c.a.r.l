use crate::error::ContractError;
use crate::slots::{betting_slot, load_slots, race_for_slot, save_slot_race};
use crate::state::{BetType, SideBet, CONFIG, RACE_ENTRIES, SIDE_BETS};
use crate::vault::{debit_vault_storage, require_no_native_funds};
use cosmwasm_std::{DepsMut, Env, MessageInfo, Response, Uint128};

pub fn execute_place_side_bet(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    bet_type: crate::state::BetType,
    amount: Uint128,
    pick: Option<cosmwasm_std::Addr>,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    if amount.is_zero() {
        return Err(ContractError::InvalidAmount {});
    }

    let config = CONFIG.load(deps.storage)?;
    let ctx = load_slots(deps.as_ref())?;
    let slot = betting_slot(&ctx, env.block.time, config.test_mode)
        .ok_or(ContractError::WrongPhase {})?;
    let mut race = race_for_slot(&ctx, slot).clone();

    let race_id = race.current_race_id;
    if bet_type == BetType::RacerVictory {
        let racer = pick.clone().ok_or(ContractError::InvalidRacerPick {})?;
        RACE_ENTRIES
            .may_load(deps.storage, (race_id, racer.clone()))?
            .ok_or(ContractError::InvalidRacerPick {})?;
    } else if pick.is_some() {
        return Err(ContractError::InvalidRacerPick {});
    }

    let key = (race_id, info.sender.clone());
    if SIDE_BETS.has(deps.storage, key.clone()) {
        return Err(ContractError::AlreadyBet {});
    }

    debit_vault_storage(deps.storage, &info.sender, amount, env.block.time)?;

    let bet = SideBet {
        bettor: info.sender.clone(),
        bet_type,
        amount,
        pick,
        claimed: false,
    };
    SIDE_BETS.save(deps.storage, key, &bet)?;

    race.total_bet_pool = race
        .total_bet_pool
        .checked_add(amount)
        .map_err(|_| ContractError::InvalidAmount {})?;
    save_slot_race(&mut deps, slot, race)?;

    Ok(Response::new()
        .add_attribute("action", "place_side_bet")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("bettor", info.sender))
}
