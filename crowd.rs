use crate::error::ContractError;
use crate::phases::{is_crowd_reveal_open, user_reveal_allowed, MAX_CROWD_ENTROPY};
use crate::settlement::crowd_salt_commitment;
use crate::slots::{crowd_commit_slot, load_slots, race_for_slot, save_slot_race};
use crate::state::{CrowdEntropy, CONFIG, CROWD_ENTROPY, RACE_GLOBAL, SIDE_BETS};
use crate::vault::require_no_native_funds;
use cosmwasm_std::{Binary, DepsMut, Env, MessageInfo, Response};

pub fn execute_commit_crowd_entropy(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    commitment: Binary,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    if commitment.is_empty() {
        return Err(ContractError::InvalidCommitment {});
    }

    let config = CONFIG.load(deps.storage)?;
    let ctx = load_slots(deps.as_ref())?;
    let slot = crowd_commit_slot(&ctx, env.block.time, config.test_mode)
        .ok_or(ContractError::WrongPhase {})?;
    let mut race = race_for_slot(&ctx, slot).clone();

    let race_id = race.current_race_id;
    let bet_key = (race_id, info.sender.clone());
    if !SIDE_BETS.has(deps.storage, bet_key) {
        return Err(ContractError::NoSideBet {});
    }

    let crowd_key = (race_id, info.sender.clone());
    if CROWD_ENTROPY.has(deps.storage, crowd_key.clone()) {
        return Err(ContractError::AlreadyCrowdCommitted {});
    }
    if race.crowd_commit_count >= MAX_CROWD_ENTROPY {
        return Err(ContractError::CrowdEntropyFull {});
    }

    let row = CrowdEntropy {
        bettor: info.sender.clone(),
        commitment,
        revealed_salt: None,
        committed_at: env.block.time,
    };
    CROWD_ENTROPY.save(deps.storage, crowd_key, &row)?;
    race.crowd_commit_count += 1;
    save_slot_race(&mut deps, slot, race)?;

    Ok(Response::new()
        .add_attribute("action", "commit_crowd_entropy")
        .add_attribute("bettor", info.sender)
        .add_attribute("race_id", race_id.to_string()))
}

pub fn execute_reveal_crowd_entropy(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    salt: String,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    if salt.is_empty() {
        return Err(ContractError::InvalidCommitment {});
    }

    let config = CONFIG.load(deps.storage)?;
    let race = RACE_GLOBAL.load(deps.storage)?;
    if !is_crowd_reveal_open(env.block.time, &race, config.test_mode) {
        return Err(ContractError::RevealWindowClosed {});
    }

    let key = (race.current_race_id, info.sender.clone());
    let mut row = CROWD_ENTROPY
        .may_load(deps.storage, key.clone())?
        .ok_or(ContractError::NotCrowdCommitted {})?;

    if row.revealed_salt.is_some() {
        return Err(ContractError::AlreadyRevealed {});
    }

    if !user_reveal_allowed(env.block.time, row.committed_at) {
        return Err(ContractError::RevealDelayNotElapsed {});
    }

    let expected = crowd_salt_commitment(&salt);
    if row.commitment != expected {
        return Err(ContractError::InvalidCommitment {});
    }

    row.revealed_salt = Some(salt);
    CROWD_ENTROPY.save(deps.storage, key, &row)?;

    Ok(Response::new()
        .add_attribute("action", "reveal_crowd_entropy")
        .add_attribute("bettor", info.sender))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phases::compute_test_phases;
    use crate::state::{BetType, SideBet, RaceGlobal};
    use cosmwasm_std::testing::{mock_dependencies, mock_env};
    use cosmwasm_std::{Addr, MessageInfo, Timestamp, Uint128};

    fn setup_race(deps: &mut cosmwasm_std::OwnedDeps<cosmwasm_std::testing::MockStorage, cosmwasm_std::testing::MockApi, cosmwasm_std::testing::MockQuerier>) {
        let schedule = compute_test_phases(Timestamp::from_seconds(1_000));
        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 1,
                    total_runners: 1,
                    total_entry_pool: Uint128::new(10_000),
                    total_bet_pool: Uint128::new(5_000),
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
                },
            )
            .unwrap();
        CONFIG
            .save(
                deps.as_mut().storage,
                &crate::state::Config {
                    admin: Addr::unchecked("admin"),
                    denom: "uatom".into(),
                    chicken_nft_address: Addr::unchecked("chicken"),
                    newt_nft_address: Addr::unchecked("newt"),
                    penguin_nft_address: Addr::unchecked("penguin"),
                    fly_nft_address: Addr::unchecked("fly"),
                    frog_nft_address: Addr::unchecked("frog"),
                    bull_nft_address: Addr::unchecked("bull"),
                    fox_nft_address: Addr::unchecked("fox"),
                    duck_nft_addresses: vec![],
                    manta_nft_address: None,
                    shrimp_nft_addresses: vec![],
                    newt_nft_addresses: vec![],
                    sloth_nft_address: None,
                    moth_nft_address: None,
                    snail_nft_address: None,
                    steer_nft_address: None,
                    goat_nft_address: None,
                    kitty_nft_addresses: vec![],
                    entry_fee: Uint128::new(10_000),
                    test_mode: true,
                },
            )
            .unwrap();
    }

    #[test]
    fn commit_requires_side_bet() {
        let mut deps = mock_dependencies();
        setup_race(&mut deps);
        let mut env = mock_env();
        env.block.time = Timestamp::from_seconds(1_500);

        let salt = "crowd-secret";
        let commitment = crowd_salt_commitment(salt);
        let err = execute_commit_crowd_entropy(
            deps.as_mut(),
            env,
            MessageInfo {
                sender: Addr::unchecked("bettor"),
                funds: vec![],
            },
            commitment,
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::NoSideBet {}));
    }

    #[test]
    fn commit_and_reveal_round_trip() {
        let mut deps = mock_dependencies();
        setup_race(&mut deps);
        SIDE_BETS
            .save(
                deps.as_mut().storage,
                (1u64, Addr::unchecked("bettor")),
                &SideBet {
                    bettor: Addr::unchecked("bettor"),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::new(1_000),
                    pick: None,
                    claimed: false,
                },
            )
            .unwrap();

        let mut env = mock_env();
        env.block.time = Timestamp::from_seconds(1_500);
        let salt = "crowd-secret";
        let commitment = crowd_salt_commitment(salt);
        execute_commit_crowd_entropy(
            deps.as_mut(),
            env.clone(),
            MessageInfo {
                sender: Addr::unchecked("bettor"),
                funds: vec![],
            },
            commitment,
        )
        .unwrap();

        env.block.time = Timestamp::from_seconds(1_850);
        execute_reveal_crowd_entropy(
            deps.as_mut(),
            env,
            MessageInfo {
                sender: Addr::unchecked("bettor"),
                funds: vec![],
            },
            salt.to_string(),
        )
        .unwrap();

        let row = CROWD_ENTROPY
            .load(deps.as_ref().storage, (1u64, Addr::unchecked("bettor")))
            .unwrap();
        assert_eq!(row.revealed_salt.as_deref(), Some(salt));
    }
}
