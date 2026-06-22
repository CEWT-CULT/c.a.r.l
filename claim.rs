use crate::error::ContractError;
use crate::msg::Cw721ExecuteMsg;
use crate::settlement::compute_wager_payout;
use crate::state::{RACE_ENTRIES, RACE_GLOBAL, RACE_SIDE_BET_SETTLEMENT, SIDE_BETS};
use crate::vault::{credit_vault, require_no_native_funds};
use cosmwasm_std::{
    to_json_binary, DepsMut, Env, MessageInfo, Response, WasmMsg,
};

/// Pull-pattern NFT return — entries persist by `race_id` so claims work long after settlement.
pub fn execute_claim_racer_nft(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    race_id: u64,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;

    let key = (race_id, info.sender.clone());
    let mut entry = RACE_ENTRIES
        .may_load(deps.storage, key.clone())?
        .ok_or(ContractError::NotEntered {})?;

    if entry.nft_claimed {
        return Err(ContractError::AlreadyClaimed {});
    }
    if entry.final_rank.is_none() {
        return Err(ContractError::NotClaimable {});
    }

    // Current race must be settled; historical races remain claimable once ranked.
    let race = RACE_GLOBAL.load(deps.storage)?;
    if race_id == race.current_race_id && !race.is_settled {
        return Err(ContractError::NotSettled {});
    }

    entry.nft_claimed = true;
    RACE_ENTRIES.save(deps.storage, key, &entry)?;

    let transfer = WasmMsg::Execute {
        contract_addr: entry.nft_contract.to_string(),
        msg: to_json_binary(&Cw721ExecuteMsg::TransferNft {
            recipient: info.sender.to_string(),
            token_id: entry.nft_id.clone(),
        })?,
        funds: vec![],
    };

    Ok(Response::new()
        .add_message(transfer)
        .add_attribute("action", "claim_racer_nft")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("player", info.sender)
        .add_attribute("token_id", entry.nft_id))
}

/// Pull-pattern side-bet payout — each bettor pays their own gas to claim after settlement.
pub fn execute_claim_wager(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    race_id: u64,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;

    let key = (race_id, info.sender.clone());
    let mut bet = SIDE_BETS
        .may_load(deps.storage, key.clone())?
        .ok_or(ContractError::NoSideBet {})?;

    if bet.claimed {
        return Err(ContractError::WagerAlreadyClaimed {});
    }

    let race = RACE_GLOBAL.load(deps.storage)?;
    if race_id == race.current_race_id && !race.is_settled {
        return Err(ContractError::NotSettled {});
    }

    let settlement = RACE_SIDE_BET_SETTLEMENT
        .may_load(deps.storage, race_id)?
        .ok_or(ContractError::NotSettled {})?;

    let payout = compute_wager_payout(&bet, &settlement);
    if payout.is_zero() {
        return Err(ContractError::NoWagerPayout {});
    }

    bet.claimed = true;
    SIDE_BETS.save(deps.storage, key, &bet)?;
    credit_vault(deps, &info.sender, payout, env.block.time)?;

    Ok(Response::new()
        .add_attribute("action", "claim_wager")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("bettor", info.sender)
        .add_attribute("payout", payout))
}
