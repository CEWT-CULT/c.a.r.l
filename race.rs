use crate::error::ContractError;
use crate::phases::{is_reveal_open, user_reveal_allowed};
use crate::settlement::commitment_hash;
use crate::state::{RaceAction, CONFIG, RACE_ENTRIES, RACE_GLOBAL};
use crate::vault::require_no_native_funds;
use cosmwasm_std::{DepsMut, Env, MessageInfo, Response};

pub fn execute_reveal_race(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    action: RaceAction,
    salt: String,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = CONFIG.load(deps.storage)?;
    let race = RACE_GLOBAL.load(deps.storage)?;

    if !is_reveal_open(env.block.time, &race, config.test_mode) {
        return Err(ContractError::RevealWindowClosed {});
    }

    let key = (race.current_race_id, info.sender.clone());
    let mut entry = RACE_ENTRIES
        .may_load(deps.storage, key.clone())?
        .ok_or(ContractError::NotEntered {})?;

    if entry.revealed_action.is_some() {
        return Err(ContractError::AlreadyRevealed {});
    }

    if !user_reveal_allowed(env.block.time, entry.committed_at) {
        return Err(ContractError::RevealDelayNotElapsed {});
    }

    let expected = commitment_hash(&action, &salt);
    if entry.commitment != expected {
        return Err(ContractError::InvalidCommitment {});
    }

    entry.revealed_action = Some(action);
    entry.revealed_salt = Some(salt);
    RACE_ENTRIES.save(deps.storage, key, &entry)?;

    Ok(Response::new()
        .add_attribute("action", "reveal_race")
        .add_attribute("player", info.sender))
}
