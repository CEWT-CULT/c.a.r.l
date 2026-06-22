use crate::error::ContractError;
use crate::msg::EnterRaceMsg;
use crate::phases::{anchor_test_race_phases, is_entry_open, MAX_RUNNERS};
use crate::species::species_from_contract;
use crate::species::species_label;
use crate::state::{
    default_user_profile, RaceEntry, CONFIG, RACE_ENTRIES, RACE_GLOBAL, USER_PROFILES,
};
use crate::vault::{debit_vault_storage, optional_exact_native_coin};
use cosmwasm_std::{DepsMut, Env, MessageInfo, Response};
use cosmwasm_std::{from_json, Addr};

pub fn execute_receive_nft(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    sender: Addr,
    token_id: String,
    msg: cosmwasm_std::Binary,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let mut race = RACE_GLOBAL.load(deps.storage)?;

    if !is_entry_open(env.block.time, &race) {
        return Err(ContractError::WrongPhase {});
    }
    if race.total_runners >= MAX_RUNNERS {
        return Err(ContractError::RaceFull {});
    }

    let species = species_from_contract(&config, &info.sender)
        .ok_or(ContractError::InvalidNftContract {})?;

    let key = (race.current_race_id, sender.clone());
    if RACE_ENTRIES.has(deps.storage, key.clone()) {
        return Err(ContractError::AlreadyEntered {});
    }

    let inner: EnterRaceMsg = from_json(msg)?;

    let fee_from_funds = optional_exact_native_coin(&info.funds, &config.denom)?;

    if fee_from_funds == config.entry_fee {
        race.total_entry_pool = race
            .total_entry_pool
            .checked_add(config.entry_fee)
            .map_err(|_| ContractError::InvalidAmount {})?;
    } else if fee_from_funds.is_zero() {
        debit_vault_storage(deps.storage, &sender, config.entry_fee, env.block.time)?;
        race.total_entry_pool = race
            .total_entry_pool
            .checked_add(config.entry_fee)
            .map_err(|_| ContractError::InvalidAmount {})?;
    } else {
        return Err(ContractError::InvalidAmount {});
    }

    let entry = RaceEntry {
        player: sender.clone(),
        nft_contract: info.sender.clone(),
        nft_id: token_id,
        species,
        commitment: inner.commitment,
        revealed_action: None,
        revealed_salt: None,
        final_rank: None,
        nft_claimed: false,
        committed_at: env.block.time,
    };

    RACE_ENTRIES.save(deps.storage, key, &entry)?;
    let first_runner = race.total_runners == 0;
    race.total_runners += 1;
    if config.test_mode && first_runner {
        anchor_test_race_phases(&mut race, env.block.time);
    }
    RACE_GLOBAL.save(deps.storage, &race)?;

    if !USER_PROFILES.has(deps.storage, sender.clone()) {
        USER_PROFILES.save(deps.storage, sender.clone(), &default_user_profile())?;
    }

    Ok(Response::new()
        .add_attribute("action", "enter_race")
        .add_attribute("player", sender)
        .add_attribute("species", species_label(entry.species)))
}
