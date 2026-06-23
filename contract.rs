#[cfg(not(feature = "library"))]
use crate::error::ContractError;
use crate::msg::{ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg};
use crate::phases::{apply_phase_timestamps, compute_production_phases_from_block, initial_race_phases, PROD_BLOCK_SECS};
use crate::state::{Config, RaceGlobal, CONFIG, RACE_GLOBAL};
use crate::{
    betting, claim, crowd, preview, query, race, race_history, receive_nft, settlement, vault,
};

use cosmwasm_std::{entry_point, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdResult, Uint128};
use cw2::set_contract_version;

const CONTRACT_NAME: &str = "carl";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const MANTA_NFT: &str = "cosmos1gc45whyrxvrk86rnelycuqjz64hs0c6dwn7u6ln6l8p84u4qeacsqrhr9l";
const SHRIMP_NFTS: [&str; 2] = [
    "cosmos1v9qpmys6wetpcg5vspk2kavej9r9cdjqf7sxwu62w3ww546rsw3qls8ur2",
    "cosmos1vpqgq2yarym3c97j7pzfdvz9f7724vjqrujal00mvd9rumm48ydqnsag5j",
];
const EXTRA_NEWT_NFTS: [&str; 2] = [
    "cosmos1csxzghvvln5kz9spz7yrqr5rw56mw9pkze56s5v79rk48kwnqwnqeac50s",
    "cosmos14czxvxfr4qrd2m944tfef88gr87dn857ygcq06xsqpj492jgwtfq29ngtz",
];
const SLOTH_NFT: &str = "cosmos1da2fer8ag2zvpznr09sqmakqw8pc6tf9e7erm0jdt58j4zmk6v4q4jf304";
const MOTH_NFT: &str = "cosmos1lkmgwmqhfvnsh7xcpulxqryst2nwujjce4fgnqs6p0vscmz4lxxsppadxl";
const SNAIL_NFT: &str = "cosmos1nm84mjvq0vfh2rlnva7c9qw0g5mmcslsw6zr4y5ejmzu0wpfpzmqylapyf";
const STEER_NFT: &str = "cosmos13wp39rwv6dv5z6nh8rxthe8s5jwwxztg4eqehhjj63exrlk2vkfqu5zz8k";
const GOAT_NFT: &str = "cosmos1l23wswrytqnfr4w2sgsmng09zgvcr5xpu75jr88u9cyaucwv7lxsx20x8n";
const KITTY_NFTS: [&str; 3] = [
    "cosmos1xknvyy20stxecwm98zj95nu9nwd4gpxyv2ytq5nt60qug6jahw5qjr0w02",
    "cosmos1f69wge2563a6epgannl55lwpqth2tkc9a2cfqmkv2shafdcuw5wsfxyzwa",
    "cosmos1dt8sxe4yhmt742aq78unyaglhmqy59pnhap96w3mdszzc7t2g0yqu2as4l",
];

fn push_addr_unique(list: &mut Vec<cosmwasm_std::Addr>, addr: cosmwasm_std::Addr) {
    if !list.iter().any(|a| a == &addr) {
        list.push(addr);
    }
}

fn ensure_species_config(deps: &DepsMut, config: &mut Config) -> StdResult<()> {
    if config.manta_nft_address.is_none() {
        config.manta_nft_address = Some(deps.api.addr_validate(MANTA_NFT)?);
    }
    if config.shrimp_nft_addresses.is_empty() {
        config.shrimp_nft_addresses = SHRIMP_NFTS
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?;
    }
    for a in EXTRA_NEWT_NFTS {
        let addr = deps.api.addr_validate(a)?;
        if addr != config.newt_nft_address {
            push_addr_unique(&mut config.newt_nft_addresses, addr);
        }
    }
    if config.sloth_nft_address.is_none() {
        config.sloth_nft_address = Some(deps.api.addr_validate(SLOTH_NFT)?);
    }
    if config.moth_nft_address.is_none() {
        config.moth_nft_address = Some(deps.api.addr_validate(MOTH_NFT)?);
    }
    if config.snail_nft_address.is_none() {
        config.snail_nft_address = Some(deps.api.addr_validate(SNAIL_NFT)?);
    }
    if config.steer_nft_address.is_none() {
        config.steer_nft_address = Some(deps.api.addr_validate(STEER_NFT)?);
    }
    if config.goat_nft_address.is_none() {
        config.goat_nft_address = Some(deps.api.addr_validate(GOAT_NFT)?);
    }
    if config.kitty_nft_addresses.is_empty() {
        config.kitty_nft_addresses = KITTY_NFTS
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?;
    }
    Ok(())
}

/// Re-anchor an in-flight production race to the current schedule for its UTC block.
/// Preserves runners, pools, and side bets — only phase timestamps change.
fn reschedule_active_production_race(race: &mut RaceGlobal) {
    let block_end = race.phase_3_close.seconds();
    if block_end <= PROD_BLOCK_SECS {
        return;
    }
    let block_start = block_end - PROD_BLOCK_SECS;
    apply_phase_timestamps(
        race,
        compute_production_phases_from_block(block_start),
    );
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, env: Env, _msg: MigrateMsg) -> StdResult<Response> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    let mut disabled_test_mode = false;
    if let Ok(mut config) = CONFIG.load(deps.storage) {
        ensure_species_config(&deps, &mut config)?;
        if config.test_mode {
            disabled_test_mode = true;
            config.test_mode = false;
        }
        CONFIG.save(deps.storage, &config)?;
    }
    if let Ok(mut race) = RACE_GLOBAL.load(deps.storage) {
        if race.crowd_commit_close.seconds() == 0 {
            race.crowd_commit_close = race.phase_2_close;
            race.crowd_reveal_close = race.phase_3_close;
        }
        let config = CONFIG.load(deps.storage)?;
        // If a race was left settled on an older build, roll forward so the desk is live.
        if race.is_settled {
            let schedule = initial_race_phases(env.block.time, config.test_mode);
            race.current_race_id += 1;
            race.total_runners = 0;
            race.total_entry_pool = Uint128::zero();
            race.total_bet_pool = Uint128::zero();
            apply_phase_timestamps(&mut race, schedule);
            race.crowd_commit_count = 0;
            race.is_settled = false;
            race.preview_step = 0;
            race.last_preview_crank = crate::state::zero_timestamp();
        } else if disabled_test_mode {
            apply_phase_timestamps(
                &mut race,
                initial_race_phases(env.block.time, false),
            );
        } else if !config.test_mode {
            reschedule_active_production_race(&mut race);
        } else if race.phase_3_close <= race.crowd_reveal_close {
            let extra = if config.test_mode {
                crate::phases::TEST_PREVIEW_LIVE_SECS
            } else {
                crate::phases::PROD_LIVE_SECS
            };
            race.phase_3_close = cosmwasm_std::Timestamp::from_seconds(
                race.crowd_reveal_close.seconds() + extra,
            );
        }
        RACE_GLOBAL.save(deps.storage, &race)?;
    }
    Ok(Response::new()
        .add_attribute("action", "migrate")
        .add_attribute("test_mode", "false"))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let config = Config {
        admin: info.sender.clone(),
        denom: msg.denom,
        chicken_nft_address: deps.api.addr_validate(&msg.chicken_nft_address)?,
        newt_nft_address: deps.api.addr_validate(&msg.newt_nft_address)?,
        newt_nft_addresses: msg
            .newt_nft_addresses
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?,
        penguin_nft_address: deps.api.addr_validate(&msg.penguin_nft_address)?,
        fly_nft_address: deps.api.addr_validate(&msg.fly_nft_address)?,
        frog_nft_address: deps.api.addr_validate(&msg.frog_nft_address)?,
        bull_nft_address: deps.api.addr_validate(&msg.bull_nft_address)?,
        fox_nft_address: deps.api.addr_validate(&msg.fox_nft_address)?,
        duck_nft_addresses: msg
            .duck_nft_addresses
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?,
        manta_nft_address: Some(deps.api.addr_validate(&msg.manta_nft_address)?),
        shrimp_nft_addresses: msg
            .shrimp_nft_addresses
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?,
        sloth_nft_address: Some(deps.api.addr_validate(&msg.sloth_nft_address)?),
        moth_nft_address: Some(deps.api.addr_validate(&msg.moth_nft_address)?),
        snail_nft_address: Some(deps.api.addr_validate(&msg.snail_nft_address)?),
        steer_nft_address: Some(deps.api.addr_validate(&msg.steer_nft_address)?),
        goat_nft_address: Some(deps.api.addr_validate(&msg.goat_nft_address)?),
        kitty_nft_addresses: msg
            .kitty_nft_addresses
            .iter()
            .map(|a| deps.api.addr_validate(a))
            .collect::<StdResult<Vec<_>>>()?,
        entry_fee: msg.entry_fee,
        test_mode: msg.test_mode,
    };

    let schedule = initial_race_phases(env.block.time, msg.test_mode);
    let race = RaceGlobal {
        current_race_id: 1,
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

    CONFIG.save(deps.storage, &config)?;
    RACE_GLOBAL.save(deps.storage, &race)?;

    Ok(Response::new()
        .add_attribute("action", "instantiate")
        .add_attribute("test_mode", msg.test_mode.to_string()))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Deposit {} => vault::execute_deposit(deps, info, env),
        ExecuteMsg::Withdraw { amount } => vault::execute_withdraw(deps, env, info, amount),
        ExecuteMsg::ReceiveNft(cw721_msg) => {
            receive_nft::execute_receive_nft(
                deps,
                env,
                info,
                cw721_msg.sender,
                cw721_msg.token_id,
                cw721_msg.msg,
            )
        }
        ExecuteMsg::PlaceSideBet {
            bet_type,
            amount,
            pick,
        } => betting::execute_place_side_bet(deps, env, info, bet_type, amount, pick),
        ExecuteMsg::RevealRace { action, salt } => {
            race::execute_reveal_race(deps, env, info, action, salt)
        }
        ExecuteMsg::CommitCrowdEntropy { commitment } => {
            crowd::execute_commit_crowd_entropy(deps, env, info, commitment)
        }
        ExecuteMsg::RevealCrowdEntropy { salt } => {
            crowd::execute_reveal_crowd_entropy(deps, env, info, salt)
        }
        ExecuteMsg::SettleRace {} => settlement::execute_settle_race(deps, env, info),
        ExecuteMsg::CrankRacePreview {} => preview::execute_crank_race_preview(deps, env, info),
        ExecuteMsg::ClaimRacerNft { race_id } => {
            claim::execute_claim_racer_nft(deps, env, info, race_id)
        }
        ExecuteMsg::ClaimWager { race_id } => {
            claim::execute_claim_wager(deps, env, info, race_id)
        }
        ExecuteMsg::AdvanceRace {} => settlement::execute_advance_race(deps, env, info),
        ExecuteMsg::AdminSetTestPhases {
            phase_1_close,
            phase_2_close,
            phase_3_open,
            phase_3_close,
            crowd_commit_close,
            crowd_reveal_close,
        } => settlement::execute_admin_set_test_phases(
            deps,
            info,
            phase_1_close,
            phase_2_close,
            phase_3_open,
            phase_3_close,
            crowd_commit_close,
            crowd_reveal_close,
        ),
        ExecuteMsg::AdminRainOutRace {} => {
            settlement::execute_admin_rain_out_race(deps, env, info)
        }
        ExecuteMsg::AdminSetEntryFee { entry_fee } => {
            settlement::execute_admin_set_entry_fee(deps, info, entry_fee)
        }
        ExecuteMsg::PruneHistory { race_id, limit } => {
            race_history::execute_prune_history(deps, info, race_id, limit)
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => query::query_config(deps),
        QueryMsg::RaceGlobal {} => query::query_race_global(deps),
        QueryMsg::User { addr } => query::query_user(deps, addr),
        QueryMsg::RaceEntry { race_id, addr } => query::query_race_entry(deps, race_id, addr),
        QueryMsg::RaceRoster { race_id } => query::query_race_roster(deps, race_id),
        QueryMsg::SideBet { race_id, addr } => query::query_side_bet(deps, race_id, addr),
        QueryMsg::SideBetDesk { race_id } => query::query_side_bet_desk(deps, race_id),
        QueryMsg::RaceHistory { start_after, limit } => {
            query::query_race_history(deps, start_after, limit)
        }
        QueryMsg::RaceTelemetry { race_id } => query::query_race_telemetry(deps, race_id),
        QueryMsg::RacePreview { race_id } => query::query_race_preview(deps, race_id),
        QueryMsg::CurrentPhase {} => query::query_current_phase(deps, env),
        QueryMsg::SideBetSettlement { race_id } => {
            query::query_side_bet_settlement(deps, race_id)
        }
        QueryMsg::CrowdEntropy { race_id, addr } => query::query_crowd_entropy(deps, race_id, addr),
        QueryMsg::CrowdEntropyDesk { race_id } => query::query_crowd_entropy_desk(deps, race_id),
    }
}
