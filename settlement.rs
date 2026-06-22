use crate::error::ContractError;
use crate::phases::{initial_race_phases, is_settlement_open, TICKS};
use crate::species::{species_for_bet_type, ALL_SPECIES};
use crate::state::{
    default_user_profile, CrowdEntropy, RaceAction, RaceEntry, RaceGlobal, RaceResult,
    SideBet, SideBetSettlement, Species, UserProfile, CONFIG, CROWD_ENTROPY, RACE_ENTRIES,
    RACE_GLOBAL, RACE_RESULTS, RACE_SIDE_BET_SETTLEMENT, SIDE_BETS, USER_PROFILES,
};
use crate::preview::clear_race_preview;
use crate::race_history::archive_settled_race;
use crate::vault::{credit_vault, require_no_native_funds};
use cosmwasm_std::{
    Addr, BankMsg, Coin, CosmosMsg, DepsMut, Env, MessageInfo, Order, Response, Storage,
    StdResult, Uint128,
};
use cosmwasm_std::Binary;
use sha2::{Digest, Sha256};

const BASE_SPEED: u32 = 50;
const RUBBERBAND_BONUS: u32 = 12;
const SABOTEUR_LEADER_DEBUFF: u32 = 8;
/// Percent-of-pool ratios use a denominator of 100 (e.g. 20 = 20%), not basis points.
const PERCENT_DENOM: u128 = 100;
const HOUSE_CUT_PCT: u128 = 20; // 20% of each pool (entry + side bets)
const FIRST_PRIZE_PCT: u128 = 70; // 70% of entry pool to 1st place
/// 10% of total pool (entry + bets) — operational bounties at settlement.
const SETTLE_CRANK_PCT: u128 = 4;
const CROWD_COMMIT_BOUNTY_PCT: u128 = 4;
const RACER_REVEAL_BOUNTY_PCT: u128 = 2;
const TOTAL_SETTLEMENT_BOUNTY_PCT: u128 =
    SETTLE_CRANK_PCT + CROWD_COMMIT_BOUNTY_PCT + RACER_REVEAL_BOUNTY_PCT;

fn percent_of(amount: Uint128, pct: u128) -> Uint128 {
    amount.multiply_ratio(pct, PERCENT_DENOM)
}

/// Side-bet pool available for resolution after the settlement bounty peel (10% of gross pool).
pub fn net_side_bet_pool(gross_pool: Uint128) -> Uint128 {
    gross_pool.saturating_sub(percent_of(gross_pool, TOTAL_SETTLEMENT_BOUNTY_PCT))
}

/// Effective side-bet stake for settlement math (matches the bounty peel applied to the pool).
pub fn net_side_wager_amount(gross: Uint128) -> Uint128 {
    gross.multiply_ratio(
        PERCENT_DENOM.saturating_sub(TOTAL_SETTLEMENT_BOUNTY_PCT),
        PERCENT_DENOM,
    )
}

/// Entry pool split: 70% 1st place, 20% house, 10% settlement bounties (entry + side).
pub fn compute_racer_payouts(entry_pool: Uint128) -> (Uint128, Uint128) {
    if entry_pool.is_zero() {
        return (Uint128::zero(), Uint128::zero());
    }
    let house_cut = percent_of(entry_pool, HOUSE_CUT_PCT);
    let first_prize = percent_of(entry_pool, FIRST_PRIZE_PCT);
    (house_cut, first_prize)
}

/// Side-bet settlement result — entry pool accounting is separate (test reference impl).
#[cfg(test)]
pub struct SideBetResolution {
    pub credits: Vec<(Addr, Uint128)>,
    pub house_cut: Uint128,
    /// True when all wagers share one bet type — full refunds, no house take.
    pub all_bets_off: bool,
}

pub fn distinct_bet_types(bets: &[(Addr, crate::state::SideBet)]) -> usize {
    let mut seen: Vec<String> = Vec::new();
    for (_, bet) in bets {
        let key = desk_side_key(bet);
        if !seen.iter().any(|k| k == &key) {
            seen.push(key);
        }
    }
    seen.len()
}

fn desk_side_key(bet: &crate::state::SideBet) -> String {
    use crate::state::BetType;
    match &bet.bet_type {
        BetType::RacerVictory => format!(
            "racer:{}",
            bet.pick.as_ref().map(|a| a.as_str()).unwrap_or("")
        ),
        BetType::UnderdogWins => "underdog".into(),
        other => format!("species:{other:?}"),
    }
}

fn bet_wins(
    bet: &crate::state::SideBet,
    winning_species: Option<Species>,
    underdog_wins: bool,
    winning_racer: Option<&Addr>,
) -> bool {
    use crate::state::BetType;
    match &bet.bet_type {
        BetType::UnderdogWins => underdog_wins,
        BetType::RacerVictory => bet
            .pick
            .as_ref()
            .zip(winning_racer)
            .map(|(pick, winner)| pick == winner)
            .unwrap_or(false),
        _ => species_for_bet_type(&bet.bet_type)
            .map(|s| winning_species == Some(s))
            .unwrap_or(false),
    }
}

/// No counterparty on the desk — e.g. every bettor picked Newts only.
pub fn is_one_sided_market(bets: &[(Addr, crate::state::SideBet)]) -> bool {
    distinct_bet_types(bets) <= 1
}

pub fn sha256_bytes(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

pub fn crowd_salt_commitment(salt: &str) -> Binary {
    Binary::from(sha256_bytes(salt.as_bytes()))
}

/// Master seed from race id + sorted `(addr, commitment)` for entries and crowd.
/// Reveals do not affect the seed — only commitments bind the random tape.
/// Block time/height are excluded so preview cranks and settlement share the same outcome.
pub fn derive_master_seed(
    race_id: u64,
    entries: &[(Addr, RaceEntry)],
    crowd: &[(Addr, crate::state::CrowdEntropy)],
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(race_id.to_be_bytes());

    let mut entries_sorted = entries.to_vec();
    entries_sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (addr, entry) in entries_sorted {
        hasher.update(addr.as_bytes());
        hasher.update(entry.commitment.as_slice());
    }

    hasher.update(b"CROWD");
    let mut crowd_sorted = crowd.to_vec();
    crowd_sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (addr, row) in crowd_sorted {
        hasher.update(addr.as_bytes());
        hasher.update(row.commitment.as_slice());
    }

    hasher.finalize().to_vec()
}

/// Split `pool` equally across `recipients`; returns undistributed dust (integer division).
fn distribute_equal_vault_bounties(
    mut deps: DepsMut,
    env: &Env,
    recipients: &[Addr],
    pool: Uint128,
) -> Result<Uint128, ContractError> {
    if pool.is_zero() || recipients.is_empty() {
        return Ok(pool);
    }
    let each = pool / Uint128::from(recipients.len() as u128);
    if each.is_zero() {
        return Ok(pool);
    }
    let distributed = each * Uint128::from(recipients.len() as u128);
    for addr in recipients {
        credit_vault(deps.branch(), addr, each, env.block.time)?;
    }
    Ok(pool.saturating_sub(distributed))
}

fn seed_byte_at(seed: &[u8], index: usize) -> u8 {
    if seed.is_empty() {
        (index as u8).wrapping_mul(0x9d)
    } else {
        seed[index % seed.len()]
    }
}

pub fn action_label(action: &RaceAction) -> &'static str {
    match action {
        RaceAction::Saboteur => "saboteur",
        RaceAction::Cheerleader => "cheerleader",
        RaceAction::Wildcard => "wildcard",
    }
}

pub fn commitment_hash(action: &RaceAction, salt: &str) -> Binary {
    let payload = format!("{}:{}", action_label(action), salt);
    Binary::from(sha256_bytes(payload.as_bytes()))
}

fn action_modifier(action: &RaceAction, seed_byte: u8) -> i32 {
    match action {
        RaceAction::Saboteur => 0,
        RaceAction::Cheerleader => 6,
        RaceAction::Wildcard => {
            if seed_byte % 2 == 0 {
                18
            } else {
                -8
            }
        }
    }
}

/// Top ~25% of the pack by cumulative distance (inverse of rubberband cutoff).
fn leader_cutoff(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        ((n + 3) / 4).max(1).min(n)
    }
}

pub fn run_physics(
    entries: &[(Addr, RaceEntry, UserProfile)],
    master_seed: &[u8],
) -> Vec<(Addr, RaceResult)> {
    let n = entries.len();
    if n == 0 {
        return vec![];
    }

    let mut distances: Vec<(Addr, [u128; TICKS], u128)> = entries
        .iter()
        .map(|(addr, entry, profile)| {
            if entry.revealed_action.is_none() {
                return (addr.clone(), [0u128; TICKS], 0u128);
            }
            let talents = profile.speed_talents;
            let mut tick_distances = [0u128; TICKS];
            let action = entry.revealed_action.as_ref().expect("forfeits filtered");

            for tick in 0..TICKS {
                let seed_byte = seed_byte_at(master_seed, tick) as u32;
                let modifier = action_modifier(action, seed_byte_at(master_seed, tick + 1));
                let velocity = (BASE_SPEED as i32
                    + talents as i32
                    + (seed_byte % 15) as i32
                    + modifier)
                    .max(1) as u32;

                tick_distances[tick] = velocity as u128;
            }
            (addr.clone(), tick_distances, 0u128)
        })
        .collect();

    let saboteur_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, (_, entry, _))| {
            matches!(entry.revealed_action.as_ref(), Some(RaceAction::Saboteur))
        })
        .map(|(i, _)| i)
        .collect();

    let forfeit_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, (_, entry, _))| entry.revealed_action.is_none())
        .map(|(i, _)| i)
        .collect();

    let bottom_cutoff = (n * 3 / 4) as usize;
    let leaders_cutoff = leader_cutoff(n);
    for tick in 0..TICKS {
        let mut pack_order: Vec<(usize, u128)> = distances
            .iter()
            .enumerate()
            .filter(|(i, _)| !forfeit_indices.contains(i))
            .map(|(i, (_, ticks, _))| {
                let cumulative: u128 = ticks[0..=tick].iter().sum();
                (i, cumulative)
            })
            .collect();
        pack_order.sort_by(|(ia, da), (ib, db)| {
            db.cmp(da)
                .then_with(|| entries[*ia].0.cmp(&entries[*ib].0))
        });

        // One debuff per leader per tick when any saboteur is in the race — never stack per saboteur.
        if !saboteur_indices.is_empty() {
            for (rank_idx, (runner_idx, _)) in pack_order.iter().enumerate() {
                if rank_idx < leaders_cutoff && !saboteur_indices.contains(runner_idx) {
                    let tick_vel = &mut distances[*runner_idx].1[tick];
                    *tick_vel = tick_vel
                        .saturating_sub(SABOTEUR_LEADER_DEBUFF as u128)
                        .max(1);
                }
            }
        }

        // Skip catch-up when the active pack is deadlocked — avoids addr-order rubber-band wins.
        let pack_tied = pack_order
            .first()
            .map(|(_, lead)| pack_order.iter().all(|(_, d)| *d == *lead))
            .unwrap_or(true);
        if !pack_tied {
            for (rank_idx, (runner_idx, _)) in pack_order.iter().enumerate() {
                if rank_idx >= bottom_cutoff {
                    distances[*runner_idx].1[tick] += RUBBERBAND_BONUS as u128;
                }
            }
        }
    }

    for (_, ticks, total) in distances.iter_mut() {
        *total = ticks.iter().sum();
    }

    distances
        .into_iter()
        .map(|(addr, tick_distances, total_distance)| {
            (
                addr,
                RaceResult {
                    tick_distances,
                    total_distance,
                },
            )
        })
        .collect()
}

fn rank_runners(results: &[(Addr, RaceResult)]) -> Vec<(Addr, u32)> {
    let mut sorted = results.to_vec();
    sorted.sort_by(|a, b| {
        b.1.total_distance
            .cmp(&a.1.total_distance)
            .then_with(|| a.0.cmp(&b.0))
    });
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, (addr, _))| (addr, (i as u32) + 1))
        .collect()
}

fn has_revealed_runner(entries: &[(Addr, RaceEntry)]) -> bool {
    entries
        .iter()
        .any(|(_, entry)| entry.revealed_action.is_some())
}

pub fn execute_settle_race(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = CONFIG.load(deps.storage)?;
    let mut race = RACE_GLOBAL.load(deps.storage)?;

    if race.is_settled {
        return Err(ContractError::AlreadySettled {});
    }
    if !is_settlement_open(env.block.time, &race) {
        return Err(ContractError::SettlementWindowClosed {});
    }

    let race_id = race.current_race_id;
    let prefix = RACE_ENTRIES.prefix(race_id);

    let entries_with_profiles: Vec<(Addr, RaceEntry, UserProfile)> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .map(|(addr, entry)| {
            let profile = USER_PROFILES
                .may_load(deps.storage, addr.clone())?
                .unwrap_or_else(default_user_profile);
            Ok((addr, entry, profile))
        })
        .collect::<StdResult<Vec<_>>>()?;

    let entries: Vec<(Addr, RaceEntry)> = entries_with_profiles
        .iter()
        .map(|(addr, entry, _)| (addr.clone(), entry.clone()))
        .collect();

    if entries.is_empty() {
        let mut response =
            finalize_rain_out_settlement(deps, env, race, &config, race_id, "settle_race")?;
        response
            .attributes
            .push(cosmwasm_std::Attribute::new("empty_race", "true"));
        return Ok(response);
    }

    if !has_revealed_runner(&entries) {
        let mut response =
            finalize_rain_out_settlement(deps, env, race, &config, race_id, "settle_race")?;
        response.attributes.push(cosmwasm_std::Attribute::new("no_reveals", "true"));
        return Ok(response);
    }

    let crowd: Vec<(Addr, CrowdEntropy)> = CROWD_ENTROPY
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();

    let master_seed = derive_master_seed(race_id, &entries, &crowd);

    let physics_results = run_physics(&entries_with_profiles, &master_seed);
    let ranks = rank_runners(&physics_results);

    for (addr, rank) in &ranks {
        let key = (race_id, addr.clone());
        if let Some(mut entry) = RACE_ENTRIES.may_load(deps.storage, key.clone())? {
            entry.final_rank = Some(*rank);
            RACE_ENTRIES.save(deps.storage, key, &entry)?;
        }
        if let Some((_, result)) = physics_results.iter().find(|(a, _)| a == addr) {
            RACE_RESULTS.save(deps.storage, (race_id, addr.clone()), result)?;
        }
    }

    let entry_pool = race.total_entry_pool;
    let bet_pool = race.total_bet_pool;
    let total_pool = entry_pool
        .checked_add(bet_pool)
        .unwrap_or(entry_pool);
    let side_bet_pool = net_side_bet_pool(bet_pool);

    let settle_bounty = percent_of(total_pool, SETTLE_CRANK_PCT);
    let crowd_bounty = percent_of(total_pool, CROWD_COMMIT_BOUNTY_PCT);
    let reveal_bounty = percent_of(total_pool, RACER_REVEAL_BOUNTY_PCT);

    let crowd_recipients: Vec<Addr> = crowd.iter().map(|(addr, _)| addr.clone()).collect();
    let reveal_recipients: Vec<Addr> = entries
        .iter()
        .filter(|(_, entry)| entry.revealed_action.is_some())
        .map(|(addr, _)| addr.clone())
        .collect();

    let (entry_house, first_prize) = compute_racer_payouts(entry_pool);

    if let Some((first_addr, _)) = ranks.first() {
        if !first_prize.is_zero() {
            credit_vault(deps.branch(), first_addr, first_prize, env.block.time)?;
        }
    }

    let (side_settlement, all_bets_off, species_tie_broken) = resolve_side_bets(
        deps.branch(),
        race_id,
        &entries,
        &ranks,
        side_bet_pool,
        &master_seed,
    )?;
    let side_house = side_settlement.house_cut;
    let zero_payout_bets_closed =
        finalize_zero_payout_side_bets(deps.branch(), race_id, &side_settlement)?;

    let mut total_house = entry_house
        .checked_add(side_house)
        .unwrap_or(entry_house);

    if !settle_bounty.is_zero() {
        credit_vault(deps.branch(), &info.sender, settle_bounty, env.block.time)?;
    }

    let crowd_remainder = distribute_equal_vault_bounties(
        deps.branch(),
        &env,
        &crowd_recipients,
        crowd_bounty,
    )?;
    let reveal_remainder = distribute_equal_vault_bounties(
        deps.branch(),
        &env,
        &reveal_recipients,
        reveal_bounty,
    )?;
    total_house = total_house
        .checked_add(crowd_remainder)
        .unwrap_or(total_house);
    total_house = total_house
        .checked_add(reveal_remainder)
        .unwrap_or(total_house);

    let mut msgs: Vec<CosmosMsg> = vec![];
    if !total_house.is_zero() {
        msgs.push(BankMsg::Send {
            to_address: config.admin.to_string(),
            amount: vec![Coin {
                denom: config.denom.clone(),
                amount: total_house,
            }],
        }
        .into());
    }

    race.is_settled = true;
    RACE_GLOBAL.save(deps.storage, &race)?;

    clear_race_preview(deps.branch(), race_id)?;

    let winner = ranks.first().map(|(addr, _)| addr.clone());
    archive_settled_race(deps.branch(), race_id, &race, winner, env.block.time, false)?;

    roll_next_race(deps.branch(), env.clone())?;
    let next = RACE_GLOBAL.load(deps.storage)?;

    Ok(Response::new()
        .add_messages(msgs)
        .add_attribute("action", "settle_race")
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("advanced_to_race", next.current_race_id.to_string())
        .add_attribute("runners", entries.len().to_string())
        .add_attribute("all_bets_off", all_bets_off.to_string())
        .add_attribute("species_tie_broken", species_tie_broken.to_string())
        .add_attribute("settle_bounty_paid", settle_bounty)
        .add_attribute("crowd_bounty_paid", crowd_bounty.saturating_sub(crowd_remainder))
        .add_attribute("crowd_bounty_recipients", crowd_recipients.len().to_string())
        .add_attribute("reveal_bounty_paid", reveal_bounty.saturating_sub(reveal_remainder))
        .add_attribute("reveal_bounty_recipients", reveal_recipients.len().to_string())
        .add_attribute("slashed_side_bets", side_settlement.slashed_bettors.len().to_string())
        .add_attribute("zero_payout_bets_closed", zero_payout_bets_closed.to_string())
        .add_attribute("crank_operator", info.sender))
}

fn bet_type_wins(
    bet: &crate::state::SideBet,
    winning_species: Option<Species>,
    underdog_wins: bool,
    winning_racer: Option<&Addr>,
) -> bool {
    bet_wins(bet, winning_species, underdog_wins, winning_racer)
}

/// Deterministic uniform index in `0..n` from seed (rejection sampling — no modulo bias).
pub fn d420_pick_index(seed: &[u8], n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let n = n as u32;
    let limit = (65_536 / n) * n;
    let mut round = 0u32;
    loop {
        let block = tie_break_block(seed, round);
        let mut i = 0usize;
        while i + 1 < block.len() {
            let value = u16::from_be_bytes([block[i], block[i + 1]]) as u32;
            if value < limit {
                return (value % n) as usize;
            }
            i += 2;
        }
        round += 1;
    }
}

fn tie_break_block(seed: &[u8], round: u32) -> Vec<u8> {
    if round == 0 {
        return seed.to_vec();
    }
    let mut material = Vec::with_capacity(seed.len() + 18);
    material.extend_from_slice(b"carl/tie-break/v1");
    material.extend_from_slice(seed);
    material.extend_from_slice(&round.to_be_bytes());
    sha256_bytes(&material)
}

/// Species with the most top-half finishers wins tribal desk; ties broken by d420.
pub fn resolve_winning_species(
    species_counts: &[(Species, u32)],
    tie_break_seed: &[u8],
) -> (Option<Species>, bool) {
    if species_counts.is_empty() {
        return (None, false);
    }
    let max = species_counts.iter().map(|(_, c)| *c).max().unwrap_or(0);
    let leaders: Vec<Species> = species_counts
        .iter()
        .filter(|(_, c)| *c == max)
        .map(|(s, _)| *s)
        .collect();
    if leaders.len() == 1 {
        (Some(leaders[0]), false)
    } else {
        let idx = d420_pick_index(tie_break_seed, leaders.len());
        (Some(leaders[idx]), true)
    }
}

/// Side-bet pool: 20% house rake from the **losing** wagers only; winners keep stake + pro-rata share of the loser pool.
#[cfg(test)]
pub fn settle_side_bets(
    bets: &[(Addr, crate::state::SideBet)],
    bet_pool: Uint128,
    winning_species: Option<Species>,
    underdog_wins: bool,
    winning_racer: Option<Addr>,
) -> SideBetResolution {
    let empty = SideBetResolution {
        credits: vec![],
        house_cut: Uint128::zero(),
        all_bets_off: false,
    };

    if bet_pool.is_zero() || bets.is_empty() {
        return empty;
    }

    if is_one_sided_market(bets) {
        let credits: Vec<(Addr, Uint128)> = bets
            .iter()
            .map(|(addr, bet)| (addr.clone(), net_side_wager_amount(bet.amount)))
            .collect();
        return SideBetResolution {
            credits,
            house_cut: Uint128::zero(),
            all_bets_off: true,
        };
    }

    let winning: Vec<&(Addr, crate::state::SideBet)> = bets
        .iter()
        .filter(|(_, bet)| {
            bet_type_wins(
                bet,
                winning_species,
                underdog_wins,
                winning_racer.as_ref(),
            )
        })
        .collect();

    if winning.is_empty() {
        return SideBetResolution {
            credits: vec![],
            house_cut: bet_pool,
            all_bets_off: false,
        };
    }

    let losing_pool: Uint128 = bets
        .iter()
        .filter(|(_, bet)| {
            !bet_type_wins(
                bet,
                winning_species,
                underdog_wins,
                winning_racer.as_ref(),
            )
        })
        .map(|(_, bet)| net_side_wager_amount(bet.amount))
        .fold(Uint128::zero(), |acc, amt| acc.checked_add(amt).unwrap_or(acc));

    let house_cut = losing_pool.multiply_ratio(HOUSE_CUT_PCT, PERCENT_DENOM);
    let loser_contribution = losing_pool.checked_sub(house_cut).unwrap_or_default();

    let total_winning_wagers: Uint128 = winning
        .iter()
        .map(|(_, bet)| net_side_wager_amount(bet.amount))
        .fold(Uint128::zero(), |acc, amt| acc.checked_add(amt).unwrap_or(acc));

    if total_winning_wagers.is_zero() {
        return SideBetResolution {
            credits: vec![],
            house_cut: bet_pool,
            all_bets_off: false,
        };
    }

    let mut credits: Vec<(Addr, Uint128)> = winning
        .iter()
        .map(|(addr, bet)| {
            let stake = net_side_wager_amount(bet.amount);
            let bonus = if loser_contribution.is_zero() {
                Uint128::zero()
            } else {
                loser_contribution.multiply_ratio(stake.u128(), total_winning_wagers.u128())
            };
            let payout = stake.checked_add(bonus).unwrap_or(stake);
            (addr.clone(), payout)
        })
        .collect();

    let distributed: Uint128 = credits
        .iter()
        .map(|(_, p)| *p)
        .fold(Uint128::zero(), |acc, p| acc.checked_add(p).unwrap_or(acc));
    let expected = bet_pool.checked_sub(house_cut).unwrap_or_default();
    if distributed < expected {
        let remainder = expected.checked_sub(distributed).unwrap_or_default();
        if let Some((_, largest)) = credits.iter_mut().max_by_key(|(_, p)| *p) {
            *largest = largest.checked_add(remainder).unwrap_or(*largest);
        }
    }

    SideBetResolution {
        credits,
        house_cut,
        all_bets_off: false,
    }
}

fn resolve_side_bets(
    deps: DepsMut,
    race_id: u64,
    entries: &[(Addr, RaceEntry)],
    ranks: &[(Addr, u32)],
    bet_pool: Uint128,
    tie_break_seed: &[u8],
) -> Result<(SideBetSettlement, bool, bool), ContractError> {
    let empty = SideBetSettlement {
        winning_species: None,
        underdog_wins: false,
        winning_racer: None,
        all_bets_off: false,
        rained_out: false,
        loser_contribution: Uint128::zero(),
        total_winning_wagers: Uint128::zero(),
        house_cut: Uint128::zero(),
        remainder: Uint128::zero(),
        remainder_recipient: None,
        slashed_bettors: vec![],
    };

    if bet_pool.is_zero() {
        return Ok((empty, false, false));
    }

    let slashed_bettors = slashed_side_bet_bettors(deps.storage, race_id, entries)?;
    let slashed_set: std::collections::HashSet<Addr> = slashed_bettors.iter().cloned().collect();

    let winning_racer = ranks.first().map(|(addr, _)| addr.clone());

    let species_counts: Vec<(Species, u32)> = ALL_SPECIES
        .iter()
        .map(|s| (*s, count_species_in_top_half(entries, ranks, *s)))
        .collect();

    let (winning_species, species_tie_broken) =
        resolve_winning_species(&species_counts, tie_break_seed);

    let underdog_wins = ranks.iter().any(|(addr, rank)| {
        *rank <= 2
            && USER_PROFILES
                .may_load(deps.storage, addr.clone())
                .ok()
                .flatten()
                .unwrap_or_else(default_user_profile)
                .level
                < 5
    });

    let prefix = SIDE_BETS.prefix(race_id);
    let mut desk_keys: Vec<String> = Vec::new();
    let mut bet_count: u32 = 0;
    let mut total_winning_wagers = Uint128::zero();
    let mut losing_pool = Uint128::zero();
    let mut max_winning_stake = Uint128::zero();
    let mut remainder_recipient: Option<Addr> = None;

    for item in prefix.range(deps.storage, None, None, Order::Ascending) {
        let (addr, bet) = item?;
        bet_count += 1;
        let stake = net_side_wager_amount(bet.amount);
        let key = desk_side_key(&bet);
        if !desk_keys.iter().any(|k| k == &key) {
            desk_keys.push(key);
        }
        if slashed_set.contains(&addr) {
            losing_pool = losing_pool
                .checked_add(stake)
                .unwrap_or(losing_pool);
            continue;
        }
        let wins = bet_type_wins(
            &bet,
            winning_species,
            underdog_wins,
            winning_racer.as_ref(),
        );
        if wins {
            total_winning_wagers = total_winning_wagers
                .checked_add(stake)
                .unwrap_or(total_winning_wagers);
            if stake > max_winning_stake {
                max_winning_stake = stake;
                remainder_recipient = Some(addr);
            }
        } else {
            losing_pool = losing_pool
                .checked_add(stake)
                .unwrap_or(losing_pool);
        }
    }

    let all_bets_off = desk_keys.len() <= 1 && bet_count > 0;
    let slashed_stake = slashed_stake_total(deps.storage, race_id, &slashed_bettors)?;

    let settlement = if all_bets_off {
        SideBetSettlement {
            winning_species,
            underdog_wins,
            winning_racer: winning_racer.clone(),
            all_bets_off: true,
            rained_out: false,
            loser_contribution: Uint128::zero(),
            total_winning_wagers: Uint128::zero(),
            house_cut: slashed_stake,
            remainder: Uint128::zero(),
            remainder_recipient: None,
            slashed_bettors: slashed_bettors.clone(),
        }
    } else if total_winning_wagers.is_zero() {
        SideBetSettlement {
            winning_species,
            underdog_wins,
            winning_racer: winning_racer.clone(),
            all_bets_off: false,
            rained_out: false,
            loser_contribution: Uint128::zero(),
            total_winning_wagers: Uint128::zero(),
            house_cut: bet_pool,
            remainder: Uint128::zero(),
            remainder_recipient: None,
            slashed_bettors: slashed_bettors.clone(),
        }
    } else {
        let house_cut = losing_pool.multiply_ratio(HOUSE_CUT_PCT, PERCENT_DENOM);
        let loser_contribution = losing_pool.checked_sub(house_cut).unwrap_or_default();
        let expected_winner_pool = bet_pool.checked_sub(house_cut).unwrap_or_default();

        let mut total_bonus = Uint128::zero();
        for item in prefix.range(deps.storage, None, None, Order::Ascending) {
            let (addr, bet) = item?;
            if slashed_set.contains(&addr) {
                continue;
            }
            if bet_type_wins(
                &bet,
                winning_species,
                underdog_wins,
                winning_racer.as_ref(),
            ) {
                let stake = net_side_wager_amount(bet.amount);
                let bonus = if loser_contribution.is_zero() || total_winning_wagers.is_zero() {
                    Uint128::zero()
                } else {
                    loser_contribution
                        .multiply_ratio(stake.u128(), total_winning_wagers.u128())
                };
                total_bonus = total_bonus.checked_add(bonus).unwrap_or(total_bonus);
            }
        }

        let distributed = total_winning_wagers
            .checked_add(total_bonus)
            .unwrap_or(total_winning_wagers);
        let remainder = if distributed < expected_winner_pool {
            expected_winner_pool
                .checked_sub(distributed)
                .unwrap_or_default()
        } else {
            Uint128::zero()
        };

        SideBetSettlement {
            winning_species,
            underdog_wins,
            winning_racer: winning_racer.clone(),
            all_bets_off: false,
            rained_out: false,
            loser_contribution,
            total_winning_wagers,
            house_cut,
            remainder,
            remainder_recipient: if remainder.is_zero() {
                None
            } else {
                remainder_recipient
            },
            slashed_bettors: slashed_bettors.clone(),
        }
    };

    RACE_SIDE_BET_SETTLEMENT.save(deps.storage, race_id, &settlement)?;

    Ok((settlement, all_bets_off, species_tie_broken))
}

/// Side-bettors who entered but withheld SET, or committed crowd salt but never revealed.
fn slashed_side_bet_bettors(
    storage: &dyn Storage,
    race_id: u64,
    entries: &[(Addr, RaceEntry)],
) -> StdResult<Vec<Addr>> {
    let mut slashed: Vec<Addr> = Vec::new();

    for (addr, entry) in entries {
        if entry.revealed_action.is_some() {
            continue;
        }
        if SIDE_BETS.has(storage, (race_id, addr.clone())) {
            slashed.push(addr.clone());
        }
    }

    for item in CROWD_ENTROPY
        .prefix(race_id)
        .range(storage, None, None, Order::Ascending)
    {
        let (addr, row) = item?;
        if row.revealed_salt.is_some() {
            continue;
        }
        if SIDE_BETS.has(storage, (race_id, addr.clone())) && !slashed.iter().any(|a| a == &addr) {
            slashed.push(addr);
        }
    }

    Ok(slashed)
}

fn slashed_stake_total(
    storage: &dyn Storage,
    race_id: u64,
    slashed: &[Addr],
) -> StdResult<Uint128> {
    let mut total = Uint128::zero();
    for addr in slashed {
        if let Some(bet) = SIDE_BETS.may_load(storage, (race_id, addr.clone()))? {
            total = total
                .checked_add(net_side_wager_amount(bet.amount))
                .unwrap_or(total);
        }
    }
    Ok(total)
}

/// Pull-pattern payout for a single bettor — mirrors `settle_side_bets` per-address math.
pub fn compute_wager_payout(bet: &SideBet, settlement: &SideBetSettlement) -> Uint128 {
    if settlement
        .slashed_bettors
        .iter()
        .any(|a| a == &bet.bettor)
    {
        return Uint128::zero();
    }

    if settlement.all_bets_off || settlement.rained_out {
        if settlement.rained_out {
            return bet.amount;
        }
        return net_side_wager_amount(bet.amount);
    }

    if !bet_type_wins(
        bet,
        settlement.winning_species,
        settlement.underdog_wins,
        settlement.winning_racer.as_ref(),
    ) {
        return Uint128::zero();
    }

    if settlement.total_winning_wagers.is_zero() {
        return Uint128::zero();
    }

    let stake = net_side_wager_amount(bet.amount);
    let bonus = if settlement.loser_contribution.is_zero() {
        Uint128::zero()
    } else {
        settlement.loser_contribution.multiply_ratio(
            stake.u128(),
            settlement.total_winning_wagers.u128(),
        )
    };
    let mut payout = stake.checked_add(bonus).unwrap_or(stake);

    if settlement.remainder_recipient.as_ref() == Some(&bet.bettor) {
        payout = payout
            .checked_add(settlement.remainder)
            .unwrap_or(payout);
    }

    payout
}

/// Mark losing/slashed side bets as claimed at settlement so they do not linger until prune.
fn finalize_zero_payout_side_bets(
    deps: DepsMut,
    race_id: u64,
    settlement: &SideBetSettlement,
) -> StdResult<u32> {
    let prefix = SIDE_BETS.prefix(race_id);
    let bets: Vec<(Addr, SideBet)> = prefix
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();

    let mut closed = 0u32;
    for (addr, mut bet) in bets {
        if bet.claimed {
            continue;
        }
        if compute_wager_payout(&bet, settlement).is_zero() {
            bet.claimed = true;
            SIDE_BETS.save(deps.storage, (race_id, addr), &bet)?;
            closed += 1;
        }
    }
    Ok(closed)
}

fn count_species_in_top_half(
    entries: &[(Addr, RaceEntry)],
    ranks: &[(Addr, u32)],
    species: Species,
) -> u32 {
    let half = (entries.len() as u32 + 1) / 2;
    ranks
        .iter()
        .filter(|(_, rank)| *rank <= half)
        .filter(|(addr, _)| {
            entries
                .iter()
                .find(|(a, _)| a == addr)
                .map(|(_, e)| e.species == species)
                .unwrap_or(false)
        })
        .count() as u32
}


pub fn execute_advance_race(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;

    let race = RACE_GLOBAL.load(deps.storage)?;
    if !race.is_settled {
        return Err(ContractError::NotSettled {});
    }

    roll_next_race(deps, env)?;

    Ok(Response::new()
        .add_attribute("action", "advance_race")
        .add_attribute("new_race_id", (race.current_race_id + 1).to_string()))
}

pub fn roll_next_race(deps: DepsMut, env: Env) -> Result<(), ContractError> {
    let config = CONFIG.load(deps.storage)?;
    let old = RACE_GLOBAL.load(deps.storage)?;
    let schedule = initial_race_phases(env.block.time, config.test_mode);

    let next = RaceGlobal {
        current_race_id: old.current_race_id + 1,
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
    RACE_GLOBAL.save(deps.storage, &next)?;
    Ok(())
}

pub fn execute_admin_set_test_phases(
    deps: DepsMut,
    info: MessageInfo,
    phase_1_close: cosmwasm_std::Timestamp,
    phase_2_close: cosmwasm_std::Timestamp,
    phase_3_open: cosmwasm_std::Timestamp,
    phase_3_close: cosmwasm_std::Timestamp,
    crowd_commit_close: Option<cosmwasm_std::Timestamp>,
    crowd_reveal_close: Option<cosmwasm_std::Timestamp>,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = CONFIG.load(deps.storage)?;
    if info.sender != config.admin || !config.test_mode {
        return Err(ContractError::Unauthorized {});
    }

    let mut race = RACE_GLOBAL.load(deps.storage)?;
    race.phase_1_close = phase_1_close;
    race.phase_2_close = phase_2_close;
    race.phase_3_open = phase_3_open;
    race.phase_3_close = phase_3_close;
    if let Some(close) = crowd_commit_close {
        race.crowd_commit_close = close;
    }
    if let Some(close) = crowd_reveal_close {
        race.crowd_reveal_close = close;
    }
    RACE_GLOBAL.save(deps.storage, &race)?;

    Ok(Response::new().add_attribute("action", "admin_set_test_phases"))
}

pub fn execute_admin_set_entry_fee(
    deps: DepsMut,
    info: MessageInfo,
    entry_fee: Uint128,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let mut config = CONFIG.load(deps.storage)?;
    if info.sender != config.admin {
        return Err(ContractError::Unauthorized {});
    }
    if entry_fee.is_zero() {
        return Err(ContractError::InvalidAmount {});
    }
    config.entry_fee = entry_fee;
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new()
        .add_attribute("action", "admin_set_entry_fee")
        .add_attribute("entry_fee", entry_fee))
}

/// Full entry + side-bet refunds (pull pattern for wagers), no house cut — used for admin rain-out and empty races.
fn rain_out_current_race(
    mut deps: DepsMut,
    env: &Env,
    race: &mut RaceGlobal,
    entry_fee: Uint128,
) -> Result<(usize, usize, Uint128, Uint128), ContractError> {
    let race_id = race.current_race_id;

    let entries: Vec<(Addr, RaceEntry)> = RACE_ENTRIES
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();
    let runner_count = entries.len();

    let mut entry_refunds = Uint128::zero();
    for (addr, mut entry) in entries {
        if !entry_fee.is_zero() {
            credit_vault(deps.branch(), &addr, entry_fee, env.block.time)?;
            entry_refunds = entry_refunds
                .checked_add(entry_fee)
                .unwrap_or(entry_refunds);
        }
        entry.final_rank = Some(crate::race_history::RAIN_OUT_RANK);
        RACE_ENTRIES.save(deps.storage, (race_id, addr), &entry)?;
    }

    let bets: Vec<(Addr, SideBet)> = SIDE_BETS
        .prefix(race_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();
    let bettor_count = bets.len();

    let settlement = SideBetSettlement {
        winning_species: None,
        underdog_wins: false,
        winning_racer: None,
        all_bets_off: true,
        rained_out: true,
        loser_contribution: Uint128::zero(),
        total_winning_wagers: Uint128::zero(),
        house_cut: Uint128::zero(),
        remainder: Uint128::zero(),
        remainder_recipient: None,
        slashed_bettors: vec![],
    };
    RACE_SIDE_BET_SETTLEMENT.save(deps.storage, race_id, &settlement)?;

    let bet_refunds = bets.iter().fold(Uint128::zero(), |acc, (_, bet)| {
        acc.checked_add(bet.amount).unwrap_or(acc)
    });

    race.is_settled = true;
    RACE_GLOBAL.save(deps.storage, race)?;

    archive_settled_race(
        deps.branch(),
        race_id,
        race,
        None,
        env.block.time,
        true,
    )?;

    Ok((runner_count, bettor_count, entry_refunds, bet_refunds))
}

fn finalize_rain_out_settlement(
    mut deps: DepsMut,
    env: Env,
    mut race: RaceGlobal,
    config: &crate::state::Config,
    race_id: u64,
    action: &str,
) -> Result<Response, ContractError> {
    let (runner_count, bettor_count, entry_refunds, bet_refunds) =
        rain_out_current_race(deps.branch(), &env, &mut race, config.entry_fee)?;

    let mut response = Response::new()
        .add_attribute("action", action)
        .add_attribute("race_id", race_id.to_string())
        .add_attribute("runners", runner_count.to_string())
        .add_attribute("entry_refunds", entry_refunds)
        .add_attribute("side_bet_refunds", bet_refunds)
        .add_attribute("bettors_refunded", bettor_count.to_string())
        .add_attribute("rained_out", "true");

    if action == "settle_race" {
        clear_race_preview(deps.branch(), race_id)?;
        roll_next_race(deps.branch(), env.clone())?;
        let next = RACE_GLOBAL.load(deps.storage)?;
        response = response.add_attribute("advanced_to_race", next.current_race_id.to_string());
    }

    Ok(response)
}

/// Admin-only emergency close: 100% entry + side-bet refunds, NFTs become claimable.
pub fn execute_admin_rain_out_race(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
) -> Result<Response, ContractError> {
    require_no_native_funds(&info.funds)?;
    let config = CONFIG.load(deps.storage)?;
    if info.sender != config.admin {
        return Err(ContractError::Unauthorized {});
    }

    let race = RACE_GLOBAL.load(deps.storage)?;
    if race.is_settled {
        return Err(ContractError::AlreadySettled {});
    }

    let race_id = race.current_race_id;
    finalize_rain_out_settlement(
        deps,
        env,
        race,
        &config,
        race_id,
        "admin_rain_out_race",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{BetType, Config};

    fn zip_entries_profiles(
        entries: &[(Addr, RaceEntry)],
        profiles: &[(Addr, UserProfile)],
    ) -> Vec<(Addr, RaceEntry, UserProfile)> {
        entries
            .iter()
            .map(|(addr, entry)| {
                let profile = profiles
                    .iter()
                    .find(|(a, _)| a == addr)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(default_user_profile);
                (addr.clone(), entry.clone(), profile)
            })
            .collect()
    }

    #[test]
    fn commitment_is_deterministic() {
        let h1 = commitment_hash(&RaceAction::Saboteur, "salt123");
        let h2 = commitment_hash(&RaceAction::Saboteur, "salt123");
        assert_eq!(h1, h2);
    }

    #[test]
    fn rubberband_targets_cumulative_trailers_not_slow_segment_leaders() {
        let leader = Addr::unchecked("leader");
        let trailer = Addr::unchecked("trailer");
        let action = RaceAction::Cheerleader;
        let entries = vec![
            (
                leader.clone(),
                RaceEntry {
                    player: leader.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".to_string(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: Some(action.clone()),
                    revealed_salt: Some("a".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
            (
                trailer.clone(),
                RaceEntry {
                    player: trailer.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".to_string(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(action.clone()),
                    revealed_salt: Some("b".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
        ];
        let leader_talents = 25u32;
        let mut leader_profile = default_user_profile();
        leader_profile.speed_talents = leader_talents;
        let profiles = vec![
            (leader.clone(), leader_profile),
            (trailer.clone(), default_user_profile()),
        ];
        let seed = sha256_bytes(&[2u8, 14, 14, 14, 14, 14, 14, 14, 14, 14]);

        let results = run_physics(&zip_entries_profiles(&entries, &profiles), &seed);
        let leader_result = &results.iter().find(|(a, _)| a == &leader).unwrap().1;
        let trailer_result = &results.iter().find(|(a, _)| a == &trailer).unwrap().1;

        assert!(leader_result.total_distance > trailer_result.total_distance);

        for tick in 0..TICKS {
            let seed_byte = seed[tick % seed.len()] as u32;
            let mod_byte = action_modifier(&action, seed[(tick + 1) % seed.len()]) as u32;
            let leader_base =
                (BASE_SPEED + leader_talents + (seed_byte % 15) + mod_byte) as u128;
            let trailer_base = (BASE_SPEED + (seed_byte % 15) + mod_byte) as u128;
            assert_eq!(
                leader_result.tick_distances[tick],
                leader_base,
                "pack leader must not receive rubberband on tick {tick}"
            );
            assert_eq!(
                trailer_result.tick_distances[tick],
                trailer_base + RUBBERBAND_BONUS as u128,
                "trailer should receive rubberband on tick {tick}"
            );
        }
    }

    #[test]
    fn saboteur_debuffs_pack_leaders_not_self() {
        let leader = Addr::unchecked("leader");
        let saboteur = Addr::unchecked("saboteur");
        let cheer = RaceAction::Cheerleader;
        let sabotage = RaceAction::Saboteur;
        let entries = vec![
            (
                leader.clone(),
                RaceEntry {
                    player: leader.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".to_string(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: Some(cheer.clone()),
                    revealed_salt: Some("a".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
            (
                saboteur.clone(),
                RaceEntry {
                    player: saboteur.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".to_string(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(sabotage.clone()),
                    revealed_salt: Some("b".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
        ];
        let leader_talents = 25u32;
        let mut leader_profile = default_user_profile();
        leader_profile.speed_talents = leader_talents;
        let profiles = vec![
            (leader.clone(), leader_profile),
            (saboteur.clone(), default_user_profile()),
        ];
        let seed = sha256_bytes(&[3u8, 14, 14, 14, 14, 14, 14, 14, 14, 14]);

        let results = run_physics(&zip_entries_profiles(&entries, &profiles), &seed);
        let leader_result = &results.iter().find(|(a, _)| a == &leader).unwrap().1;
        let saboteur_result = &results.iter().find(|(a, _)| a == &saboteur).unwrap().1;

        for tick in 0..TICKS {
            let seed_byte = seed[tick % seed.len()] as u32;
            let cheer_mod = action_modifier(&cheer, seed[(tick + 1) % seed.len()]) as i32;
            let sabotage_mod =
                action_modifier(&sabotage, seed[(tick + 1) % seed.len()]) as i32;
            let leader_base = (BASE_SPEED as i32
                + leader_talents as i32
                + (seed_byte % 15) as i32
                + cheer_mod)
                .max(1) as u128;
            let saboteur_base =
                (BASE_SPEED as i32 + (seed_byte % 15) as i32 + sabotage_mod).max(1) as u128;

            assert_eq!(
                leader_result.tick_distances[tick],
                leader_base.saturating_sub(SABOTEUR_LEADER_DEBUFF as u128).max(1),
                "pack leader should be sabotaged on tick {tick}"
            );
            assert_eq!(
                saboteur_result.tick_distances[tick],
                saboteur_base + RUBBERBAND_BONUS as u128,
                "saboteur keeps base speed plus rubberband on tick {tick}"
            );
        }
    }

    #[test]
    fn saboteur_debuff_does_not_stack_with_multiple_saboteurs() {
        let leader = Addr::unchecked("leader");
        let cheer = RaceAction::Cheerleader;
        let sabotage = RaceAction::Saboteur;

        let mut entries = vec![(
            leader.clone(),
            RaceEntry {
                player: leader.clone(),
                nft_contract: Addr::unchecked("nft"),
                nft_id: "0".to_string(),
                species: Species::Chicken,
                commitment: Binary::default(),
                revealed_action: Some(cheer.clone()),
                revealed_salt: Some("leader".to_string()),
                final_rank: None,
                nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
            },
        )];

        let mut profiles = vec![{
            let mut p = default_user_profile();
            p.speed_talents = 30;
            (leader.clone(), p)
        }];

        for i in 0..10 {
            let addr = Addr::unchecked(format!("sab{i}"));
            entries.push((
                addr.clone(),
                RaceEntry {
                    player: addr.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: format!("s{i}"),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(sabotage.clone()),
                    revealed_salt: Some(format!("s{i}")),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ));
            profiles.push((addr, default_user_profile()));
        }

        let seed = sha256_bytes(&[4u8, 14, 14, 14, 14, 14, 14, 14, 14, 14]);
        let results = run_physics(&zip_entries_profiles(&entries, &profiles), &seed);
        let leader_result = &results.iter().find(|(a, _)| a == &leader).unwrap().1;

        for tick in 0..TICKS {
            let seed_byte = seed[tick % seed.len()] as u32;
            let cheer_mod = action_modifier(&cheer, seed[(tick + 1) % seed.len()]) as i32;
            let leader_base = (BASE_SPEED as i32 + 30 + (seed_byte % 15) as i32 + cheer_mod).max(1)
                as u128;
            let expected = leader_base
                .saturating_sub(SABOTEUR_LEADER_DEBUFF as u128)
                .max(1);

            assert_eq!(
                leader_result.tick_distances[tick], expected,
                "leader should take at most one saboteur debuff on tick {tick}, not stack per saboteur"
            );
            assert!(
                leader_result.tick_distances[tick] > 1 || leader_base <= 1,
                "syndicate saboteurs must not stun-lock leader to 1 on tick {tick}"
            );
        }
    }

    #[test]
    fn master_seed_uses_crowd_commitment_only() {
        use cosmwasm_std::Addr;
        use crate::state::CrowdEntropy;

        let addr = Addr::unchecked("bettor");
        let salt = "crowd-salt";
        let commitment = crowd_salt_commitment(salt);
        let revealed = CrowdEntropy {
            bettor: addr.clone(),
            commitment: commitment.clone(),
            revealed_salt: Some(salt.to_string()),
            committed_at: crate::state::zero_timestamp(),
        };
        let withheld = CrowdEntropy {
            bettor: addr.clone(),
            commitment,
            revealed_salt: None,
            committed_at: crate::state::zero_timestamp(),
        };
        let base = derive_master_seed(1, &[], &[]);
        let with_commitment = derive_master_seed(1, &[], &[(addr.clone(), revealed.clone())]);
        let with_withheld = derive_master_seed(1, &[], &[(addr, withheld)]);
        assert_ne!(base, with_commitment);
        assert_eq!(with_commitment, with_withheld);
    }

    #[test]
    fn derive_master_seed_is_deterministic_and_domain_separated() {
        let player = Addr::unchecked("player");
        let entry = RaceEntry {
            player: player.clone(),
            nft_contract: Addr::unchecked("nft"),
            nft_id: "1".to_string(),
            species: Species::Chicken,
            commitment: commitment_hash(&RaceAction::Cheerleader, "salt"),
            revealed_action: Some(RaceAction::Cheerleader),
            revealed_salt: Some("salt".to_string()),
            final_rank: None,
            nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
        };
        let rows = vec![(player.clone(), entry.clone())];
        let a = derive_master_seed(1, &rows, &[]);
        let b = derive_master_seed(1, &rows, &[]);
        let c = derive_master_seed(2, &rows, &[]);
        assert_eq!(a.len(), 32);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn reveal_timing_does_not_change_master_seed() {
        let a = Addr::unchecked("a");
        let revealed = RaceEntry {
            player: a.clone(),
            nft_contract: Addr::unchecked("nft"),
            nft_id: "1".to_string(),
            species: Species::Chicken,
            commitment: commitment_hash(&RaceAction::Cheerleader, "s"),
            revealed_action: Some(RaceAction::Cheerleader),
            revealed_salt: Some("s".to_string()),
            final_rank: None,
            nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
        };
        let withheld = RaceEntry {
            revealed_action: None,
            revealed_salt: None,
            ..revealed.clone()
        };
        let seed_revealed = derive_master_seed(1, &[(a.clone(), revealed)], &[]);
        let seed_withheld = derive_master_seed(1, &[(a, withheld)], &[]);
        assert_eq!(seed_revealed, seed_withheld);
    }

    #[test]
    fn run_physics_forfeit_has_zero_distance() {
        let a = Addr::unchecked("a");
        let b = Addr::unchecked("b");
        let entries = vec![
            (
                a.clone(),
                RaceEntry {
                    player: a.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".to_string(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
                default_user_profile(),
            ),
            (
                b.clone(),
                RaceEntry {
                    player: b.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".to_string(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(RaceAction::Cheerleader),
                    revealed_salt: Some("s".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
                default_user_profile(),
            ),
        ];
        let seed = derive_master_seed(1, &entries.iter().map(|(a, e, _)| (a.clone(), e.clone())).collect::<Vec<_>>(), &[]);
        let results = run_physics(&entries, &seed);
        let forfeit = results.iter().find(|(addr, _)| addr == &a).unwrap();
        let racing = results.iter().find(|(addr, _)| addr == &b).unwrap();
        assert_eq!(forfeit.1.total_distance, 0);
        assert!(racing.1.total_distance > 0);
    }

    #[test]
    fn run_physics_settles_when_no_one_revealed() {
        let a = Addr::unchecked("a");
        let b = Addr::unchecked("b");
        let entries = vec![
            (
                a.clone(),
                RaceEntry {
                    player: a.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".to_string(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
            (
                b.clone(),
                RaceEntry {
                    player: b.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".to_string(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
        ];
        let profiles = vec![
            (a.clone(), default_user_profile()),
            (b.clone(), default_user_profile()),
        ];
        let seed = derive_master_seed(7, &entries, &[]);
        let results = run_physics(&zip_entries_profiles(&entries, &profiles), &seed);
        assert_eq!(results.len(), 2);
        assert_eq!(seed.len(), 32);
        assert!(results.iter().all(|(_, r)| r.total_distance == 0));
    }

    #[test]
    fn rank_runners_tie_breaks_to_lower_address() {
        let a = Addr::unchecked("a");
        let b = Addr::unchecked("b");
        let results = vec![
            (
                b.clone(),
                RaceResult {
                    tick_distances: [0; TICKS],
                    total_distance: 100,
                },
            ),
            (
                a.clone(),
                RaceResult {
                    tick_distances: [0; TICKS],
                    total_distance: 100,
                },
            ),
        ];
        let ranks = rank_runners(&results);
        assert_eq!(ranks[0], (a, 1));
        assert_eq!(ranks[1], (b, 2));
    }

    #[test]
    fn run_physics_tied_pack_skips_rubberband() {
        let a = Addr::unchecked("a");
        let b = Addr::unchecked("b");
        let cheer = RaceAction::Cheerleader;
        let entries = vec![
            (
                a.clone(),
                RaceEntry {
                    player: a.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".to_string(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: Some(cheer.clone()),
                    revealed_salt: Some("sa".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
                default_user_profile(),
            ),
            (
                b.clone(),
                RaceEntry {
                    player: b.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".to_string(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(cheer),
                    revealed_salt: Some("sb".to_string()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
                default_user_profile(),
            ),
        ];
        let seed = derive_master_seed(3, &entries.iter().map(|(a, e, _)| (a.clone(), e.clone())).collect::<Vec<_>>(), &[]);
        let results = run_physics(&entries, &seed);
        let dist_a = results.iter().find(|(addr, _)| addr == &a).unwrap().1.total_distance;
        let dist_b = results.iter().find(|(addr, _)| addr == &b).unwrap().1.total_distance;
        assert_eq!(dist_a, dist_b);
        let ranks = rank_runners(&results);
        assert_eq!(ranks[0].0, a);
    }

    #[test]
    fn seed_byte_at_never_panics_on_empty_slice() {
        for tick in 0..TICKS {
            let _ = seed_byte_at(&[], tick);
            let _ = seed_byte_at(&[], tick + 1);
        }
    }

    #[test]
    fn physics_is_deterministic() {
        let addr = Addr::unchecked("player1");
        let entries = vec![(
            addr.clone(),
            RaceEntry {
                player: addr.clone(),
                nft_contract: Addr::unchecked("nft"),
                nft_id: "1".to_string(),
                species: Species::Chicken,
                commitment: Binary::default(),
                revealed_action: Some(RaceAction::Cheerleader),
                revealed_salt: Some("secret".to_string()),
                final_rank: None,
                nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
            },
        )];
        let profiles = vec![(addr, default_user_profile())];
        let seed = sha256_bytes(b"test");
        let zipped = zip_entries_profiles(&entries, &profiles);
        let r1 = run_physics(&zipped, &seed);
        let r2 = run_physics(&zipped, &seed);
        assert_eq!(r1[0].1.total_distance, r2[0].1.total_distance);
    }

    #[test]
    fn master_seed_ignores_plaintext_reveals() {
        let addr = Addr::unchecked("player1");
        let action = RaceAction::Saboteur;
        let commitment = commitment_hash(&action, "secret-salt");

        let unrevealed = RaceEntry {
            player: addr.clone(),
            nft_contract: Addr::unchecked("nft"),
            nft_id: "42".to_string(),
            species: Species::Newt,
            commitment: commitment.clone(),
            revealed_action: None,
            revealed_salt: None,
            final_rank: None,
            nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
        };
        let revealed = RaceEntry {
            revealed_action: Some(action),
            revealed_salt: Some("secret-salt".to_string()),
            ..unrevealed.clone()
        };

        let seed_before = derive_master_seed(1, &[(addr.clone(), unrevealed)], &[]);
        let seed_after = derive_master_seed(1, &[(addr, revealed)], &[]);
        assert_eq!(seed_before, seed_after);
    }

    #[test]
    fn withholding_reveal_penalizes_withholder_not_peers_when_seed_fixed() {
        let saboteur = Addr::unchecked("saboteur");
        let victim = Addr::unchecked("victim");
        let victim_entry = RaceEntry {
            player: victim.clone(),
            nft_contract: Addr::unchecked("nft"),
            nft_id: "1".to_string(),
            species: Species::Chicken,
            commitment: commitment_hash(&RaceAction::Cheerleader, "v"),
            revealed_action: Some(RaceAction::Cheerleader),
            revealed_salt: Some("v".to_string()),
            final_rank: None,
            nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
        };
        let saboteur_revealed = RaceEntry {
            player: saboteur.clone(),
            nft_contract: Addr::unchecked("nft"),
            nft_id: "2".to_string(),
            species: Species::Newt,
            commitment: commitment_hash(&RaceAction::Saboteur, "s"),
            revealed_action: Some(RaceAction::Saboteur),
            revealed_salt: Some("s".to_string()),
            final_rank: None,
            nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
        };
        let saboteur_withheld = RaceEntry {
            revealed_action: None,
            revealed_salt: None,
            ..saboteur_revealed.clone()
        };

        let profile = default_user_profile();
        let entries_revealed = vec![
            (saboteur.clone(), saboteur_revealed.clone()),
            (victim.clone(), victim_entry.clone()),
        ];
        let entries_withheld = vec![
            (saboteur.clone(), saboteur_withheld),
            (victim.clone(), victim_entry.clone()),
        ];
        let master = derive_master_seed(1, &entries_revealed, &[]);

        let zipped_reveal = vec![
            (victim.clone(), victim_entry.clone(), profile.clone()),
            (saboteur.clone(), saboteur_revealed, profile.clone()),
        ];
        let zipped_withhold = vec![
            (victim.clone(), victim_entry, profile.clone()),
            (
                saboteur.clone(),
                entries_withheld[0].1.clone(),
                profile,
            ),
        ];

        let r1 = run_physics(&zipped_reveal, &master);
        let r2 = run_physics(&zipped_withhold, &master);

        let sab_revealed = r1.iter().find(|(a, _)| a == &saboteur).unwrap().1.total_distance;
        let sab_withheld = r2.iter().find(|(a, _)| a == &saboteur).unwrap().1.total_distance;
        assert!(sab_withheld < sab_revealed);
    }

    #[test]
    fn species_tie_d420_breaks_to_exactly_one_winner() {
        let seed = sha256_bytes(b"d420-tie-break");
        let counts = vec![(Species::Chicken, 4), (Species::Newt, 4)];
        let (winner, tie_broken) = resolve_winning_species(&counts, &seed);
        assert!(tie_broken);
        assert!(winner.is_some());

        let (w2, t2) = resolve_winning_species(&counts, &seed);
        assert_eq!(winner, w2);
        assert!(t2);
    }

    #[test]
    fn d420_pick_index_is_deterministic_and_unbiased_by_rejection() {
        let seed = sha256_bytes(b"uniform-tie-break");
        assert_eq!(d420_pick_index(&seed, 2), d420_pick_index(&seed, 2));
        assert!(d420_pick_index(&seed, 2) < 2);
        assert_eq!(d420_pick_index(&seed, 1), 0);
    }

    #[test]
    fn species_tie_d420_does_not_pay_both_sides() {
        use crate::state::SideBet;

        let chicken_bettor = Addr::unchecked("chicken");
        let newt_bettor = Addr::unchecked("newt");
        let gross_pool = Uint128::from(10_000_000u128);
        let pool = net_side_bet_pool(gross_pool);
        let bets = vec![
            (
                chicken_bettor.clone(),
                SideBet {
                    bettor: chicken_bettor.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                newt_bettor.clone(),
                SideBet {
                    bettor: newt_bettor.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let seed = sha256_bytes(b"d420-tie-break");
        let counts = vec![(Species::Chicken, 2), (Species::Newt, 2)];
        let (winner, _) = resolve_winning_species(&counts, &seed);
        let resolution = settle_side_bets(&bets, pool, winner, false, None);

        let chicken_paid = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &chicken_bettor)
            .map(|(_, p)| *p)
            .unwrap_or_default();
        let newt_paid = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &newt_bettor)
            .map(|(_, p)| *p)
            .unwrap_or_default();

        if winner == Some(Species::Chicken) {
            assert!(chicken_paid > Uint128::zero());
            assert_eq!(newt_paid, Uint128::zero());
        } else {
            assert!(newt_paid > Uint128::zero());
            assert_eq!(chicken_paid, Uint128::zero());
        }

        let winner_paid: Uint128 = resolution.credits.iter().map(|(_, p)| *p).sum();
        assert_eq!(
            winner_paid.checked_add(resolution.house_cut).unwrap(),
            pool
        );
    }

    #[test]
    fn settlement_bounties_split_ten_percent() {
        let entry_pool = Uint128::from(100_000_000u128);
        let bet_pool = Uint128::from(50_000_000u128);
        let total = entry_pool + bet_pool;
        assert_eq!(
            total.multiply_ratio(SETTLE_CRANK_PCT, PERCENT_DENOM),
            Uint128::from(6_000_000u128)
        );
        assert_eq!(
            total.multiply_ratio(CROWD_COMMIT_BOUNTY_PCT, PERCENT_DENOM),
            Uint128::from(6_000_000u128)
        );
        assert_eq!(
            total.multiply_ratio(RACER_REVEAL_BOUNTY_PCT, PERCENT_DENOM),
            Uint128::from(3_000_000u128)
        );
        assert_eq!(TOTAL_SETTLEMENT_BOUNTY_PCT, 10);
    }

    #[test]
    fn racer_payout_first_takes_seventy_percent() {
        let entry_pool = Uint128::from(100_000_000u128); // 100 ATOM
        let (house, first) = compute_racer_payouts(entry_pool);
        assert_eq!(house, Uint128::from(20_000_000u128));
        assert_eq!(first, Uint128::from(70_000_000u128));
        assert_eq!(
            house
                .checked_add(first)
                .unwrap()
                .checked_add(entry_pool.multiply_ratio(TOTAL_SETTLEMENT_BOUNTY_PCT, PERCENT_DENOM))
                .unwrap(),
            entry_pool
        );
    }

    #[test]
    fn side_bet_payouts_are_pro_rata_not_per_address() {
        use crate::state::SideBet;

        let whale = Addr::unchecked("whale");
        let sybil = Addr::unchecked("sybil");
        let counterparty = Addr::unchecked("newt_bettor");
        let gross_pool = Uint128::from(10_000_001u128);
        let pool = net_side_bet_pool(gross_pool);

        let bets = vec![
            (
                whale.clone(),
                SideBet {
                    bettor: whale.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                sybil.clone(),
                SideBet {
                    bettor: sybil.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(1u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                counterparty.clone(),
                SideBet {
                    bettor: counterparty.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let resolution = settle_side_bets(&bets, pool, Some(Species::Chicken), false, None);
        assert!(!resolution.all_bets_off);
        let net_losing = net_side_wager_amount(Uint128::from(5_000_000u128));
        assert_eq!(
            resolution.house_cut,
            net_losing.multiply_ratio(HOUSE_CUT_PCT, PERCENT_DENOM)
        );

        let loser_contribution = net_losing.multiply_ratio(80u128, 100u128);
        let winning_stakes = net_side_wager_amount(Uint128::from(5_000_000u128))
            .checked_add(net_side_wager_amount(Uint128::one()))
            .unwrap();
        let whale_payout = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &whale)
            .map(|(_, p)| *p)
            .unwrap();
        let sybil_payout = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &sybil)
            .map(|(_, p)| *p)
            .unwrap();

        assert!(whale_payout > winning_stakes.checked_add(loser_contribution.multiply_ratio(99u128, 100u128)).unwrap_or(winning_stakes));
        assert!(sybil_payout < Uint128::from(100u128));

        let winner_paid: Uint128 = resolution.credits.iter().map(|(_, p)| *p).sum();
        assert_eq!(
            winner_paid.checked_add(resolution.house_cut).unwrap(),
            pool
        );
    }

    #[test]
    fn one_sided_side_bets_refunded_all_bets_off() {
        use crate::state::SideBet;

        let a = Addr::unchecked("a");
        let b = Addr::unchecked("b");
        let gross_pool = Uint128::from(8_000_000u128);
        let pool = net_side_bet_pool(gross_pool);
        let bets = vec![
            (
                a.clone(),
                SideBet {
                    bettor: a.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                b.clone(),
                SideBet {
                    bettor: b.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(3_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let resolution = settle_side_bets(&bets, pool, Some(Species::Newt), false, None);
        assert!(resolution.all_bets_off);
        assert_eq!(resolution.house_cut, Uint128::zero());
        assert_eq!(resolution.credits.len(), 2);
        assert_eq!(
            resolution.credits.iter().map(|(_, p)| *p).sum::<Uint128>(),
            pool
        );
    }

    #[test]
    fn entry_and_side_house_cuts_are_independent() {
        let entry_pool = Uint128::from(100_000_000u128);
        let (entry_house, first) = compute_racer_payouts(entry_pool);
        assert_eq!(entry_house, Uint128::from(20_000_000u128));
        assert_eq!(first, Uint128::from(70_000_000u128));

        use crate::state::SideBet;
        let bets = vec![
            (
                Addr::unchecked("c"),
                SideBet {
                    bettor: Addr::unchecked("c"),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(4_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                Addr::unchecked("n"),
                SideBet {
                    bettor: Addr::unchecked("n"),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(4_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];
        let gross_side = Uint128::from(8_000_000u128);
        let side_pool = net_side_bet_pool(gross_side);
        let side = settle_side_bets(&bets, side_pool, Some(Species::Chicken), false, None);
        let net_losing = net_side_wager_amount(Uint128::from(4_000_000u128));
        assert_eq!(
            side.house_cut,
            net_losing.multiply_ratio(HOUSE_CUT_PCT, PERCENT_DENOM)
        );
        assert_eq!(
            entry_house.checked_add(side.house_cut).unwrap(),
            Uint128::from(20_720_000u128)
        );
    }

    #[test]
    fn dust_counterparty_cannot_rake_whale_stake() {
        use crate::state::SideBet;

        let whale = Addr::unchecked("whale");
        let dust = Addr::unchecked("dust");
        let gross_pool = Uint128::from(10_000_001u128);
        let pool = net_side_bet_pool(gross_pool);
        let bets = vec![
            (
                whale.clone(),
                SideBet {
                    bettor: whale.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(10_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                dust.clone(),
                SideBet {
                    bettor: dust.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(1u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let resolution = settle_side_bets(&bets, pool, Some(Species::Chicken), false, None);
        assert!(!resolution.all_bets_off);
        // House rake is 20% of the net loser pool (dust rounds to zero net stake).
        assert_eq!(resolution.house_cut, Uint128::zero());

        let whale_payout = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &whale)
            .map(|(_, p)| *p)
            .unwrap();
        assert_eq!(whale_payout, pool);
    }

    #[test]
    fn sybil_army_cannot_drain_whale_side_bet() {
        use crate::state::SideBet;

        let whale = Addr::unchecked("whale");
        let whale_bet = Uint128::from(5_000_000u128);

        let counterparty = Addr::unchecked("newt_whale");
        let mut bets = vec![
            (
                whale.clone(),
                SideBet {
                    bettor: whale.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: whale_bet,
                    pick: None,
                    claimed: false,
                },
            ),
            (
                counterparty.clone(),
                SideBet {
                    bettor: counterparty.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(10_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        for i in 0..500 {
            let burner = Addr::unchecked(format!("burner{i}"));
            bets.push((
                burner.clone(),
                SideBet {
                    bettor: burner,
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::one(),
                    pick: None,
                    claimed: false,
                },
            ));
        }

        let gross_pool = Uint128::from(115_000_500u128);
        let pool = net_side_bet_pool(gross_pool);
        let resolution = settle_side_bets(&bets, pool, Some(Species::Chicken), false, None);
        let winner_payout_total = pool.checked_sub(resolution.house_cut).unwrap_or_default();
        let whale_payout = resolution
            .credits
            .iter()
            .find(|(a, _)| a == &whale)
            .map(|(_, p)| *p)
            .unwrap();

        assert!(whale_payout > winner_payout_total.multiply_ratio(85u128, 100u128));
    }

    #[test]
    fn pull_payout_matches_batch_settlement() {
        use crate::state::SideBet;

        let whale = Addr::unchecked("whale");
        let sybil = Addr::unchecked("sybil");
        let counterparty = Addr::unchecked("newt_bettor");
        let gross_pool = Uint128::from(10_000_001u128);
        let pool = net_side_bet_pool(gross_pool);

        let bets = vec![
            (
                whale.clone(),
                SideBet {
                    bettor: whale.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                sybil.clone(),
                SideBet {
                    bettor: sybil.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(1u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                counterparty.clone(),
                SideBet {
                    bettor: counterparty.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(5_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let batch = settle_side_bets(&bets, pool, Some(Species::Chicken), false, None);

        let net_losing = net_side_wager_amount(Uint128::from(5_000_000u128));
        let house_cut = net_losing.multiply_ratio(HOUSE_CUT_PCT, PERCENT_DENOM);
        let loser_contribution = net_losing.checked_sub(house_cut).unwrap_or_default();
        let total_winning_wagers = net_side_wager_amount(Uint128::from(5_000_000u128))
            .checked_add(net_side_wager_amount(Uint128::one()))
            .unwrap();

        let mut settlement = SideBetSettlement {
            winning_species: Some(Species::Chicken),
            underdog_wins: false,
            winning_racer: None,
            all_bets_off: false,
            rained_out: false,
            loser_contribution,
            total_winning_wagers,
            house_cut: batch.house_cut,
            remainder: Uint128::zero(),
            remainder_recipient: None,
            slashed_bettors: vec![],
        };

        let pull_without_remainder: Uint128 = bets
            .iter()
            .map(|(_, bet)| compute_wager_payout(bet, &settlement))
            .sum();
        let batch_total: Uint128 = batch.credits.iter().map(|(_, p)| *p).sum();
        settlement.remainder = batch_total
            .checked_sub(pull_without_remainder)
            .unwrap_or_default();
        if !settlement.remainder.is_zero() {
            settlement.remainder_recipient = batch
                .credits
                .iter()
                .max_by_key(|(_, p)| *p)
                .map(|(a, _)| a.clone());
        }

        for (addr, bet) in &bets {
            let batch_payout = batch
                .credits
                .iter()
                .find(|(a, _)| a == addr)
                .map(|(_, p)| *p)
                .unwrap_or_default();
            let pull_payout = compute_wager_payout(bet, &settlement);
            assert_eq!(
                pull_payout, batch_payout,
                "payout mismatch for {addr}"
            );
        }

        let pull_total: Uint128 = bets
            .iter()
            .map(|(_, bet)| compute_wager_payout(bet, &settlement))
            .sum();
        let batch_total: Uint128 = batch.credits.iter().map(|(_, p)| *p).sum();
        assert_eq!(pull_total, batch_total);
    }

    #[test]
    fn side_bet_settlement_conserves_net_pool() {
        use crate::state::SideBet;

        let gross_pool = Uint128::from(50_000_000u128);
        let pool = net_side_bet_pool(gross_pool);
        let bets = vec![
            (
                Addr::unchecked("c"),
                SideBet {
                    bettor: Addr::unchecked("c"),
                    bet_type: BetType::ChickenVictory,
                    amount: Uint128::from(30_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
            (
                Addr::unchecked("n"),
                SideBet {
                    bettor: Addr::unchecked("n"),
                    bet_type: BetType::NewtVictory,
                    amount: Uint128::from(20_000_000u128),
                    pick: None,
                    claimed: false,
                },
            ),
        ];

        let resolution = settle_side_bets(&bets, pool, Some(Species::Chicken), false, None);
        let winner_paid: Uint128 = resolution.credits.iter().map(|(_, p)| *p).sum();
        assert_eq!(
            winner_paid.checked_add(resolution.house_cut).unwrap(),
            pool
        );
    }

    #[test]
    fn empty_race_settlement_refunds_side_bets_without_house_cut() {
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::{MessageInfo, Timestamp};
        use crate::claim;
        use crate::state::{
            BetType, Config, RaceGlobal, SideBet, CONFIG, RACE_GLOBAL, RACE_HISTORY,
            RACE_SIDE_BET_SETTLEMENT, SIDE_BETS, USERS,
        };

        let mut deps = mock_dependencies();
        let chicken_bettor = Addr::unchecked("chicken_bettor");
        let newt_bettor = Addr::unchecked("newt_bettor");
        let stake = Uint128::from(500_000_000u128);

        CONFIG
            .save(
                deps.as_mut().storage,
                &Config {
                    admin: Addr::unchecked("admin"),
                    denom: "uatom".into(),
                    chicken_nft_address: Addr::unchecked("chicken"),
                    newt_nft_address: Addr::unchecked("newt"),
                    penguin_nft_address: Addr::unchecked("penguin"),
                    fly_nft_address: Addr::unchecked("fly"),
                    frog_nft_address: Addr::unchecked("frog"),
                    bull_nft_address: Addr::unchecked("bull"),
                    fox_nft_address: Addr::unchecked("fox"),
                    duck_nft_addresses: vec![Addr::unchecked("duck")],
                    manta_nft_address: None,
                    shrimp_nft_addresses: vec![],
                    newt_nft_addresses: vec![],
                    sloth_nft_address: None,
                    moth_nft_address: None,
                    snail_nft_address: None,
                    steer_nft_address: None,
                    goat_nft_address: None,
                    kitty_nft_addresses: vec![],
                    entry_fee: Uint128::from(1_000_000u128),
                    test_mode: true,
                },
            )
            .unwrap();

        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 1,
                    total_runners: 0,
                    total_entry_pool: Uint128::zero(),
                    total_bet_pool: stake.checked_add(stake).unwrap(),
                    phase_1_close: Timestamp::from_seconds(100),
                    phase_2_close: Timestamp::from_seconds(200),
                    phase_3_open: Timestamp::from_seconds(300),
                    phase_3_close: Timestamp::from_seconds(350),
                    crowd_commit_close: Timestamp::from_seconds(200),
                    crowd_reveal_close: Timestamp::from_seconds(350),
                    crowd_commit_count: 0,
                    is_settled: false,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        for (bettor, bet_type) in [
            (chicken_bettor.clone(), BetType::ChickenVictory),
            (newt_bettor.clone(), BetType::NewtVictory),
        ] {
            SIDE_BETS
                .save(
                    deps.as_mut().storage,
                    (1u64, bettor.clone()),
                    &SideBet {
                        bettor: bettor.clone(),
                        bet_type,
                        amount: stake,
                        pick: None,
                    claimed: false,
                    },
                )
                .unwrap();
        }

        let mut env = mock_env();
        env.block.time = Timestamp::from_seconds(400);
        let info = MessageInfo {
            sender: Addr::unchecked("cranker"),
            funds: vec![],
        };

        let response = execute_settle_race(deps.as_mut(), env.clone(), info)
            .unwrap();

        assert_eq!(
            response
                .attributes
                .iter()
                .find(|a| a.key == "empty_race")
                .map(|a| a.value.as_str()),
            Some("true")
        );
        assert!(response.messages.is_empty());

        let race = RACE_GLOBAL.load(deps.as_ref().storage).unwrap();
        assert_eq!(race.current_race_id, 2);
        assert!(!race.is_settled);

        let settlement = RACE_SIDE_BET_SETTLEMENT
            .load(deps.as_ref().storage, 1)
            .unwrap();
        assert!(settlement.rained_out);
        assert!(settlement.all_bets_off);
        assert_eq!(settlement.house_cut, Uint128::zero());

        let history = RACE_HISTORY.load(deps.as_ref().storage, 1).unwrap();
        assert!(history.rained_out);

        for bettor in [chicken_bettor.clone(), newt_bettor.clone()] {
            claim::execute_claim_wager(
                deps.as_mut(),
                env.clone(),
                MessageInfo {
                    sender: bettor.clone(),
                    funds: vec![],
                },
                1,
            )
            .unwrap();
            let vault = USERS.load(deps.as_ref().storage, bettor).unwrap();
            assert_eq!(vault.deposits, stake);
        }
    }

    #[test]
    fn no_reveal_settlement_refunds_entries_and_side_bets() {
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::{Binary, MessageInfo, Timestamp};
        use crate::claim;
        use crate::race_history::RAIN_OUT_RANK;
        use crate::state::{
            BetType, Config, RaceEntry, RaceGlobal, SideBet, CONFIG, RACE_ENTRIES, RACE_GLOBAL,
            RACE_HISTORY, SIDE_BETS, USERS,
        };

        let mut deps = mock_dependencies();
        let runner_a = Addr::unchecked("runner_a");
        let runner_b = Addr::unchecked("runner_b");
        let bettor = Addr::unchecked("bettor");
        let entry_fee = Uint128::from(1_000_000u128);
        let stake = Uint128::from(500_000u128);

        CONFIG
            .save(
                deps.as_mut().storage,
                &Config {
                    admin: Addr::unchecked("admin"),
                    denom: "uatom".into(),
                    chicken_nft_address: Addr::unchecked("chicken"),
                    newt_nft_address: Addr::unchecked("newt"),
                    penguin_nft_address: Addr::unchecked("penguin"),
                    fly_nft_address: Addr::unchecked("fly"),
                    frog_nft_address: Addr::unchecked("frog"),
                    bull_nft_address: Addr::unchecked("bull"),
                    fox_nft_address: Addr::unchecked("fox"),
                    duck_nft_addresses: vec![Addr::unchecked("duck")],
                    manta_nft_address: None,
                    shrimp_nft_addresses: vec![],
                    newt_nft_addresses: vec![],
                    sloth_nft_address: None,
                    moth_nft_address: None,
                    snail_nft_address: None,
                    steer_nft_address: None,
                    goat_nft_address: None,
                    kitty_nft_addresses: vec![],
                    entry_fee,
                    test_mode: true,
                },
            )
            .unwrap();

        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 1,
                    total_runners: 2,
                    total_entry_pool: entry_fee.checked_add(entry_fee).unwrap(),
                    total_bet_pool: stake,
                    phase_1_close: Timestamp::from_seconds(100),
                    phase_2_close: Timestamp::from_seconds(200),
                    phase_3_open: Timestamp::from_seconds(300),
                    phase_3_close: Timestamp::from_seconds(350),
                    crowd_commit_close: Timestamp::from_seconds(200),
                    crowd_reveal_close: Timestamp::from_seconds(350),
                    crowd_commit_count: 0,
                    is_settled: false,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        for (runner, species) in [(runner_a.clone(), Species::Chicken), (runner_b.clone(), Species::Newt)] {
            RACE_ENTRIES
                .save(
                    deps.as_mut().storage,
                    (1u64, runner.clone()),
                    &RaceEntry {
                        player: runner.clone(),
                        nft_contract: Addr::unchecked("nft"),
                        nft_id: "1".into(),
                        species,
                        commitment: Binary::default(),
                        revealed_action: None,
                        revealed_salt: None,
                        final_rank: None,
                        nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                    },
                )
                .unwrap();
        }

        SIDE_BETS
            .save(
                deps.as_mut().storage,
                (1u64, bettor.clone()),
                &SideBet {
                    bettor: bettor.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: stake,
                    pick: None,
                    claimed: false,
                },
            )
            .unwrap();

        let mut env = mock_env();
        env.block.time = Timestamp::from_seconds(400);
        let info = MessageInfo {
            sender: Addr::unchecked("cranker"),
            funds: vec![],
        };

        let response = execute_settle_race(deps.as_mut(), env.clone(), info).unwrap();

        assert_eq!(
            response
                .attributes
                .iter()
                .find(|a| a.key == "no_reveals")
                .map(|a| a.value.as_str()),
            Some("true")
        );
        assert_eq!(
            response
                .attributes
                .iter()
                .find(|a| a.key == "rained_out")
                .map(|a| a.value.as_str()),
            Some("true")
        );

        let race = RACE_GLOBAL.load(deps.as_ref().storage).unwrap();
        assert_eq!(race.current_race_id, 2);
        assert!(!race.is_settled);

        for runner in [runner_a.clone(), runner_b.clone()] {
            let vault = USERS.load(deps.as_ref().storage, runner.clone()).unwrap();
            assert_eq!(vault.deposits, entry_fee);
            let entry = RACE_ENTRIES
                .load(deps.as_ref().storage, (1u64, runner))
                .unwrap();
            assert_eq!(entry.final_rank, Some(RAIN_OUT_RANK));
        }

        claim::execute_claim_wager(
            deps.as_mut(),
            env.clone(),
            MessageInfo {
                sender: bettor.clone(),
                funds: vec![],
            },
            1,
        )
        .unwrap();
        let bettor_vault = USERS.load(deps.as_ref().storage, bettor).unwrap();
        assert_eq!(bettor_vault.deposits, stake);

        let history = RACE_HISTORY.load(deps.as_ref().storage, 1).unwrap();
        assert!(history.rained_out);
        assert!(history.winner.is_none());
    }

    #[test]
    fn admin_rain_out_refunds_and_unlocks_claims() {
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::{Binary, MessageInfo, Timestamp};
        use crate::claim;
        use crate::race_history::RAIN_OUT_RANK;
        use crate::state::{BetType, Config, RaceGlobal, SideBet, CONFIG, RACE_ENTRIES, RACE_GLOBAL, RACE_HISTORY, SIDE_BETS, USERS};

        let mut deps = mock_dependencies();
        let admin = Addr::unchecked("admin");
        let runner = Addr::unchecked("runner");
        let bettor = Addr::unchecked("bettor");
        let entry_fee = Uint128::from(1_000_000u128);
        let bet_amount = Uint128::from(500_000u128);

        CONFIG
            .save(
                deps.as_mut().storage,
                &Config {
                    admin: admin.clone(),
                    denom: "uatom".into(),
                    chicken_nft_address: Addr::unchecked("chicken"),
                    newt_nft_address: Addr::unchecked("newt"),
                    penguin_nft_address: Addr::unchecked("penguin"),
                    fly_nft_address: Addr::unchecked("fly"),
                    frog_nft_address: Addr::unchecked("frog"),
                    bull_nft_address: Addr::unchecked("bull"),
                    fox_nft_address: Addr::unchecked("fox"),
                    duck_nft_addresses: vec![Addr::unchecked("duck")],
                    manta_nft_address: None,
                    shrimp_nft_addresses: vec![],
                    newt_nft_addresses: vec![],
                    sloth_nft_address: None,
                    moth_nft_address: None,
                    snail_nft_address: None,
                    steer_nft_address: None,
                    goat_nft_address: None,
                    kitty_nft_addresses: vec![],
                    entry_fee,
                    test_mode: true,
                },
            )
            .unwrap();

        RACE_GLOBAL
            .save(
                deps.as_mut().storage,
                &RaceGlobal {
                    current_race_id: 1,
                    total_runners: 1,
                    total_entry_pool: entry_fee,
                    total_bet_pool: bet_amount,
                    phase_1_close: Timestamp::from_seconds(100),
                    phase_2_close: Timestamp::from_seconds(200),
                    phase_3_open: Timestamp::from_seconds(300),
                    phase_3_close: Timestamp::from_seconds(350),
                    crowd_commit_close: Timestamp::from_seconds(200),
                    crowd_reveal_close: Timestamp::from_seconds(350),
                    crowd_commit_count: 0,
                    is_settled: false,
                    preview_step: 0,
                    last_preview_crank: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        RACE_ENTRIES
            .save(
                deps.as_mut().storage,
                (1u64, runner.clone()),
                &RaceEntry {
                    player: runner.clone(),
                    nft_contract: Addr::unchecked("chicken"),
                    nft_id: "42".into(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            )
            .unwrap();

        SIDE_BETS
            .save(
                deps.as_mut().storage,
                (1u64, bettor.clone()),
                &SideBet {
                    bettor: bettor.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: bet_amount,
                    pick: None,
                    claimed: false,
                },
            )
            .unwrap();

        let env = mock_env();
        let info = MessageInfo {
            sender: admin,
            funds: vec![],
        };

        execute_admin_rain_out_race(deps.as_mut(), env.clone(), info).unwrap();

        let race = RACE_GLOBAL.load(deps.as_ref().storage).unwrap();
        assert!(race.is_settled);

        let runner_vault = USERS.load(deps.as_ref().storage, runner.clone()).unwrap();
        assert_eq!(runner_vault.deposits, entry_fee);

        // Side bets use pull-pattern — bettor must claim after rain-out.
        assert_eq!(
            USERS
                .may_load(deps.as_ref().storage, bettor.clone())
                .unwrap()
                .map(|u| u.deposits)
                .unwrap_or_default(),
            Uint128::zero()
        );

        claim::execute_claim_wager(
            deps.as_mut(),
            env,
            MessageInfo {
                sender: bettor.clone(),
                funds: vec![],
            },
            1,
        )
        .unwrap();

        let bettor_vault = USERS.load(deps.as_ref().storage, bettor.clone()).unwrap();
        assert_eq!(bettor_vault.deposits, bet_amount);

        let entry = RACE_ENTRIES
            .load(deps.as_ref().storage, (1, runner))
            .unwrap();
        assert_eq!(entry.final_rank, Some(RAIN_OUT_RANK));
        assert!(!entry.nft_claimed);
        assert!(entry.final_rank.is_some() && !entry.nft_claimed);

        let history = RACE_HISTORY.load(deps.as_ref().storage, 1).unwrap();
        assert!(history.rained_out);
        assert!(history.winner.is_none());
    }

    #[test]
    fn withheld_racer_side_bet_is_slashed() {
        use cosmwasm_std::testing::mock_dependencies;
        use cosmwasm_std::Binary;

        let mut deps = mock_dependencies();
        let racer = Addr::unchecked("racer");
        let counterparty = Addr::unchecked("counterparty");
        let race_id = 1u64;
        let stake = Uint128::from(10_000_000u128);

        SIDE_BETS
            .save(
                deps.as_mut().storage,
                (race_id, racer.clone()),
                &SideBet {
                    bettor: racer.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: stake,
                    pick: None,
                    claimed: false,
                },
            )
            .unwrap();
        SIDE_BETS
            .save(
                deps.as_mut().storage,
                (race_id, counterparty.clone()),
                &SideBet {
                    bettor: counterparty.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: stake,
                    pick: None,
                    claimed: false,
                },
            )
            .unwrap();

        let entries = vec![
            (
                racer.clone(),
                RaceEntry {
                    player: racer.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "1".into(),
                    species: Species::Chicken,
                    commitment: Binary::default(),
                    revealed_action: None,
                    revealed_salt: None,
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
            (
                counterparty.clone(),
                RaceEntry {
                    player: counterparty.clone(),
                    nft_contract: Addr::unchecked("nft"),
                    nft_id: "2".into(),
                    species: Species::Newt,
                    commitment: Binary::default(),
                    revealed_action: Some(RaceAction::Cheerleader),
                    revealed_salt: Some("s".into()),
                    final_rank: None,
                    nft_claimed: false,
            committed_at: crate::state::zero_timestamp(),
                },
            ),
        ];
        let ranks = vec![(counterparty.clone(), 1u32), (racer.clone(), 2u32)];
        let pool = stake.checked_add(stake).unwrap();

        let (settlement, _, _) = resolve_side_bets(
            deps.as_mut(),
            race_id,
            &entries,
            &ranks,
            pool,
            b"seed",
        )
        .unwrap();

        assert_eq!(settlement.slashed_bettors, vec![racer.clone()]);
        assert_eq!(
            compute_wager_payout(
                &SideBet {
                    bettor: racer.clone(),
                    bet_type: BetType::ChickenVictory,
                    amount: stake,
                    pick: None,
                    claimed: false,
                },
                &settlement
            ),
            Uint128::zero()
        );
        assert!(
            compute_wager_payout(
                &SideBet {
                    bettor: counterparty.clone(),
                    bet_type: BetType::NewtVictory,
                    amount: stake,
                    pick: None,
                    claimed: false,
                },
                &settlement
            ) > stake
        );
    }

    fn sample_config(admin: Addr, test_mode: bool) -> Config {
        Config {
            admin,
            denom: "uatom".into(),
            chicken_nft_address: Addr::unchecked("chicken"),
            newt_nft_address: Addr::unchecked("newt"),
            penguin_nft_address: Addr::unchecked("penguin"),
            fly_nft_address: Addr::unchecked("fly"),
            frog_nft_address: Addr::unchecked("frog"),
            bull_nft_address: Addr::unchecked("bull"),
            fox_nft_address: Addr::unchecked("fox"),
            duck_nft_addresses: vec![Addr::unchecked("duck")],
            manta_nft_address: None,
            shrimp_nft_addresses: vec![],
            newt_nft_addresses: vec![],
            sloth_nft_address: None,
            moth_nft_address: None,
            snail_nft_address: None,
            steer_nft_address: None,
            goat_nft_address: None,
            kitty_nft_addresses: vec![],
            entry_fee: Uint128::from(1_000_000u128),
            test_mode,
        }
    }

    fn settled_race() -> RaceGlobal {
        use cosmwasm_std::Timestamp;

        RaceGlobal {
            current_race_id: 1,
            total_runners: 2,
            total_entry_pool: Uint128::from(2_000_000u128),
            total_bet_pool: Uint128::zero(),
            phase_1_close: Timestamp::from_seconds(100),
            phase_2_close: Timestamp::from_seconds(200),
            phase_3_open: Timestamp::from_seconds(300),
            phase_3_close: Timestamp::from_seconds(350),
            crowd_commit_close: Timestamp::from_seconds(200),
            crowd_reveal_close: Timestamp::from_seconds(350),
            crowd_commit_count: 0,
            is_settled: true,
            preview_step: 0,
            last_preview_crank: crate::state::zero_timestamp(),
        }
    }

    #[test]
    fn advance_race_permissionless() {
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::MessageInfo;
        use crate::state::{CONFIG, RACE_GLOBAL};

        let mut deps = mock_dependencies();
        let cranker = Addr::unchecked("anyone");

        CONFIG
            .save(deps.as_mut().storage, &sample_config(Addr::unchecked("admin"), true))
            .unwrap();
        RACE_GLOBAL
            .save(deps.as_mut().storage, &settled_race())
            .unwrap();

        let env = mock_env();
        let info = MessageInfo {
            sender: cranker,
            funds: vec![],
        };

        execute_advance_race(deps.as_mut(), env, info).unwrap();

        let race = RACE_GLOBAL.load(deps.as_ref().storage).unwrap();
        assert_eq!(race.current_race_id, 2);
        assert!(!race.is_settled);
    }

    #[test]
    fn advance_race_requires_settled_race() {
        use cosmwasm_std::testing::{mock_dependencies, mock_env};
        use cosmwasm_std::MessageInfo;
        use crate::state::{CONFIG, RACE_GLOBAL};

        let mut deps = mock_dependencies();
        let mut unsettled = settled_race();
        unsettled.is_settled = false;
        CONFIG
            .save(
                deps.as_mut().storage,
                &sample_config(Addr::unchecked("admin"), true),
            )
            .unwrap();
        RACE_GLOBAL
            .save(deps.as_mut().storage, &unsettled)
            .unwrap();

        let err = execute_advance_race(
            deps.as_mut(),
            mock_env(),
            MessageInfo {
                sender: Addr::unchecked("anyone"),
                funds: vec![],
            },
        )
        .unwrap_err();
        assert!(matches!(err, ContractError::NotSettled {}));
    }
}
