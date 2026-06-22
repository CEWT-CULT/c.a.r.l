use crate::error::ContractError;
use crate::state::{default_users, CONFIG, USERS};
use cosmwasm_std::{BankMsg, Coin, DepsMut, Env, MessageInfo, Response, Storage, Uint128};

/// Reject any native coins on execute paths that do not accept payment.
pub fn require_no_native_funds(funds: &[Coin]) -> Result<(), ContractError> {
    if funds.is_empty() {
        Ok(())
    } else {
        Err(ContractError::InvalidDenomSent {})
    }
}

/// Exactly one native coin, matching `denom`, with a non-zero amount.
pub fn require_exact_native_coin(funds: &[Coin], denom: &str) -> Result<Uint128, ContractError> {
    if funds.len() != 1 || funds[0].denom != denom {
        return Err(ContractError::InvalidDenomSent {});
    }
    let amount = funds[0].amount;
    if amount.is_zero() {
        return Err(ContractError::InvalidAmount {});
    }
    Ok(amount)
}

/// No funds (zero) or exactly one coin of `denom` — used when vault debit is the alternative.
pub fn optional_exact_native_coin(funds: &[Coin], denom: &str) -> Result<Uint128, ContractError> {
    match funds.len() {
        0 => Ok(Uint128::zero()),
        1 => {
            if funds[0].denom != denom {
                return Err(ContractError::InvalidDenomSent {});
            }
            Ok(funds[0].amount)
        }
        _ => Err(ContractError::InvalidDenomSent {}),
    }
}

pub fn execute_deposit(
    deps: DepsMut,
    info: MessageInfo,
    env: Env,
) -> Result<Response, ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let deposit_amount = require_exact_native_coin(&info.funds, &config.denom)?;
    let mut user = USERS
        .may_load(deps.storage, info.sender.clone())?
        .unwrap_or_else(default_users);

    user.deposits = user
        .deposits
        .checked_add(deposit_amount)
        .map_err(|_| ContractError::InvalidAmount {})?;
    user.last_action = Some(env.block.time);
    USERS.save(deps.storage, info.sender.clone(), &user)?;

    Ok(Response::new()
        .add_attribute("action", "deposit")
        .add_attribute("amount", deposit_amount))
}

pub fn execute_withdraw(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    amount: Uint128,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = CONFIG.load(deps.storage)?;
    let mut user = USERS
        .may_load(deps.storage, info.sender.clone())?
        .unwrap_or_else(default_users);

    if amount > user.deposits {
        return Err(ContractError::InsufficientFunds {});
    }

    user.deposits = user
        .deposits
        .checked_sub(amount)
        .map_err(|_| ContractError::InsufficientFunds {})?;
    user.last_action = Some(env.block.time);
    USERS.save(deps.storage, info.sender.clone(), &user)?;

    let transfer_msg = BankMsg::Send {
        to_address: info.sender.to_string(),
        amount: vec![Coin {
            denom: config.denom.clone(),
            amount,
        }],
    };

    Ok(Response::new()
        .add_message(transfer_msg)
        .add_attribute("action", "withdraw")
        .add_attribute("amount", amount))
}

pub fn credit_vault(
    deps: DepsMut,
    addr: &cosmwasm_std::Addr,
    amount: Uint128,
    time: cosmwasm_std::Timestamp,
) -> Result<(), ContractError> {
    credit_vault_storage(deps.storage, addr, amount, time)
}

pub fn credit_vault_storage(
    storage: &mut dyn Storage,
    addr: &cosmwasm_std::Addr,
    amount: Uint128,
    time: cosmwasm_std::Timestamp,
) -> Result<(), ContractError> {
    if amount.is_zero() {
        return Ok(());
    }
    let mut user = USERS
        .may_load(storage, addr.clone())?
        .unwrap_or_else(default_users);
    user.deposits = user
        .deposits
        .checked_add(amount)
        .map_err(|_| ContractError::InvalidAmount {})?;
    user.last_action = Some(time);
    USERS.save(storage, addr.clone(), &user)?;
    Ok(())
}

pub fn debit_vault_storage(
    storage: &mut dyn Storage,
    addr: &cosmwasm_std::Addr,
    amount: Uint128,
    time: cosmwasm_std::Timestamp,
) -> Result<(), ContractError> {
    let mut user = USERS
        .may_load(storage, addr.clone())?
        .unwrap_or_else(default_users);
    if amount > user.deposits {
        return Err(ContractError::InsufficientFunds {});
    }
    user.deposits = user
        .deposits
        .checked_sub(amount)
        .map_err(|_| ContractError::InsufficientFunds {})?;
    user.last_action = Some(time);
    USERS.save(storage, addr.clone(), &user)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::Coin;

    #[test]
    fn exact_native_coin_rejects_multiple_denoms() {
        let funds = vec![
            Coin {
                denom: "uatom".into(),
                amount: Uint128::new(1_000_000),
            },
            Coin {
                denom: "ustake".into(),
                amount: Uint128::new(500),
            },
        ];
        assert!(require_exact_native_coin(&funds, "uatom").is_err());
    }

    #[test]
    fn exact_native_coin_rejects_wrong_denom() {
        let funds = vec![Coin {
            denom: "ustake".into(),
            amount: Uint128::new(1),
        }];
        assert!(require_exact_native_coin(&funds, "uatom").is_err());
    }

    #[test]
    fn exact_native_coin_accepts_single_match() {
        let funds = vec![Coin {
            denom: "uatom".into(),
            amount: Uint128::new(2_000_000),
        }];
        let amt = require_exact_native_coin(&funds, "uatom").unwrap();
        assert_eq!(amt, Uint128::new(2_000_000));
    }

    #[test]
    fn optional_native_coin_rejects_extras() {
        let funds = vec![
            Coin {
                denom: "uatom".into(),
                amount: Uint128::new(1_000_000),
            },
            Coin {
                denom: "uatom".into(),
                amount: Uint128::new(1_000_000),
            },
        ];
        assert!(optional_exact_native_coin(&funds, "uatom").is_err());
    }

    #[test]
    fn require_no_native_funds_rejects_any_attachment() {
        let funds = vec![Coin {
            denom: "uatom".into(),
            amount: Uint128::new(1),
        }];
        assert!(require_no_native_funds(&funds).is_err());
        assert!(require_no_native_funds(&[]).is_ok());
    }
}
