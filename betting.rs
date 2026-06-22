use crate::error::ContractError;
use crate::phases::is_betting_open;
use crate::state::{BetType, SideBet, CONFIG, RACE_ENTRIES, RACE_GLOBAL, SIDE_BETS};
use crate::vault::{debit_vault_storage, require_no_native_funds};
use cosmwasm_std::{DepsMut, Env, MessageInfo, Response, Uint128};

pub fn execute_place_side_bet(
    deps: DepsMut,
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
    let mut race = RACE_GLOBAL.load(deps.storage)?;

    if !is_betting_open(env.block.time, &race, config.test_mode) {
        return Err(ContractError::WrongPhase {});
    }

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
    RACE_GLOBAL.save(deps.storage, &race)?;

    Ok(Response::new()
        .add_attribute("action", "place_side_bet")
        .add_attribute("bettor", info.sender)
        .add_attribute("amount", amount)
        .add_attribute("denom", config.denom))
}
