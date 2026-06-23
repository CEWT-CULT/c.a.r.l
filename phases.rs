use crate::msg::RacePhase;
use crate::state::RaceGlobal;
use cosmwasm_std::Timestamp;

const SECONDS_PER_MINUTE: u64 = 60;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const DAY_SECS: u64 = 86_400;
const ONE_HOUR: u64 = 3600;

/// Production: 3 races per UTC day in 8-hour blocks; final hour is live on track.
pub const PROD_RACES_PER_DAY: u64 = 3;
pub const PROD_BLOCK_SECS: u64 = 8 * ONE_HOUR;
pub const PROD_LIVE_SECS: u64 = ONE_HOUR;
/// Combined entry, side bets, and crowd commits (GET HYPED during entry).
pub const PROD_PREP_SECS: u64 = 3 * ONE_HOUR;
/// Global reveal window after prep — each actor also waits `REVEAL_DELAY_SECS` after their commit.
pub const PROD_REVEAL_GRACE_SECS: u64 = 3 * ONE_HOUR;
/// Minimum delay between a commit and that actor's reveal (anti copy-reveal).
pub const REVEAL_DELAY_SECS: u64 = 5 * SECONDS_PER_MINUTE;

pub const MAX_RUNNERS: u32 = 75;
pub const MAX_CROWD_ENTROPY: u32 = 50;
pub const TICKS: usize = 5;

/// Each test phase window (entry, crowd commit, crowd reveal).
pub const TEST_PHASE_LEN: u64 = 5 * SECONDS_PER_MINUTE;
/// Full cycle: 3 × 5-minute phases before race goes live.
pub const TEST_CYCLE_SECS: u64 = 3 * TEST_PHASE_LEN;
/// Live track window after reveals — one crank per minute in test (5 cranks max before settle opens).
pub const TEST_PREVIEW_LIVE_SECS: u64 = 5 * SECONDS_PER_MINUTE;

fn nanos_from_secs(secs: u64) -> u64 {
    secs * NANOS_PER_SECOND
}

fn ts(secs: u64) -> Timestamp {
    Timestamp::from_nanos(nanos_from_secs(secs))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseTimestamps {
    pub phase_1_close: Timestamp,
    pub phase_2_close: Timestamp,
    pub phase_3_open: Timestamp,
    pub phase_3_close: Timestamp,
    pub crowd_commit_close: Timestamp,
    pub crowd_reveal_close: Timestamp,
}

/// Idle test race — clock starts on the first NFT entry.
pub fn compute_test_phases_waiting() -> PhaseTimestamps {
    let far = Timestamp::from_nanos(u64::MAX / 2);
    PhaseTimestamps {
        phase_1_close: far,
        phase_2_close: far,
        phase_3_open: far,
        phase_3_close: far,
        crowd_commit_close: far,
        crowd_reveal_close: far,
    }
}

/// Test mode: 5m entry → 5m crowd commit → 5m reveals → race live. Side bets stay open until then.
pub fn compute_test_phases(anchor: Timestamp) -> PhaseTimestamps {
    let base = anchor.seconds();
    let entry_close = base + TEST_PHASE_LEN;
    let crowd_commit_close = base + 2 * TEST_PHASE_LEN;
    let crowd_reveal_close = base + TEST_CYCLE_SECS;
    let settlement_open = crowd_reveal_close + TEST_PREVIEW_LIVE_SECS;
    PhaseTimestamps {
        phase_2_close: ts(entry_close),
        phase_1_close: ts(crowd_reveal_close),
        phase_3_open: ts(crowd_commit_close),
        crowd_commit_close: ts(crowd_commit_close),
        crowd_reveal_close: ts(crowd_reveal_close),
        phase_3_close: ts(settlement_open),
    }
}

/// Which 8-hour UTC block `now` falls in (0, 8h, or 16h from midnight).
#[cfg(test)]
pub fn current_production_block_start(now_secs: u64) -> u64 {
    let day_start = now_secs - (now_secs % DAY_SECS);
    let block_idx = ((now_secs - day_start) / PROD_BLOCK_SECS).min(PROD_RACES_PER_DAY - 1);
    day_start + block_idx * PROD_BLOCK_SECS
}

/// Next race block — skip blocks whose prep window already closed.
pub fn production_block_start_for_new_race(now_secs: u64) -> u64 {
    let day_start = now_secs - (now_secs % DAY_SECS);
    for i in 0..PROD_RACES_PER_DAY {
        let block_start = day_start + i * PROD_BLOCK_SECS;
        if now_secs < block_start + PROD_PREP_SECS {
            return block_start;
        }
    }
    day_start + DAY_SECS
}

pub fn compute_production_phases_from_block(block_start: u64) -> PhaseTimestamps {
    let prep_close = block_start + PROD_PREP_SECS;
    let reveal_close = prep_close + PROD_REVEAL_GRACE_SECS;
    let phase_3_close = block_start + PROD_BLOCK_SECS;
    PhaseTimestamps {
        phase_2_close: ts(prep_close),
        phase_1_close: ts(reveal_close),
        phase_3_open: ts(prep_close),
        crowd_commit_close: ts(prep_close),
        crowd_reveal_close: ts(reveal_close),
        phase_3_close: ts(phase_3_close),
    }
}

/// Production: 3 × 8h blocks per UTC day — 3h prep, 3h reveal, 1h live.
#[cfg(test)]
pub fn compute_production_phases(now: Timestamp) -> PhaseTimestamps {
    compute_production_phases_from_block(current_production_block_start(now.seconds()))
}

pub fn initial_race_phases(now: Timestamp, test_mode: bool) -> PhaseTimestamps {
    if test_mode {
        compute_test_phases_waiting()
    } else {
        compute_production_phases_from_block(production_block_start_for_new_race(now.seconds()))
    }
}

pub fn apply_phase_timestamps(race: &mut RaceGlobal, schedule: PhaseTimestamps) {
    race.phase_1_close = schedule.phase_1_close;
    race.phase_2_close = schedule.phase_2_close;
    race.phase_3_open = schedule.phase_3_open;
    race.phase_3_close = schedule.phase_3_close;
    race.crowd_commit_close = schedule.crowd_commit_close;
    race.crowd_reveal_close = schedule.crowd_reveal_close;
}

pub fn anchor_test_race_phases(race: &mut RaceGlobal, anchor: Timestamp) {
    apply_phase_timestamps(race, compute_test_phases(anchor));
}

pub fn crowd_entropy_enabled(race: &RaceGlobal) -> bool {
    race.crowd_reveal_close.seconds() > race.crowd_commit_close.seconds()
}

/// Entry and crowd commit share the prep window (production).
pub fn combined_prep(race: &RaceGlobal) -> bool {
    crowd_entropy_enabled(race)
        && race.crowd_commit_close.seconds() == race.phase_2_close.seconds()
}

fn crowd_commit_start(race: &RaceGlobal) -> Timestamp {
    if combined_prep(race) {
        race.phase_2_close
    } else if race.crowd_commit_close.seconds() > race.phase_2_close.seconds() {
        race.phase_2_close
    } else {
        race.phase_1_close
    }
}

pub fn user_reveal_allowed(now: Timestamp, committed_at: Timestamp) -> bool {
    if committed_at.seconds() == 0 {
        return true;
    }
    now.seconds() >= committed_at.seconds().saturating_add(REVEAL_DELAY_SECS)
}

/// Side bets until the race goes live on the track. Test mode requires at least one runner.
pub fn is_betting_open(now: Timestamp, race: &RaceGlobal, test_mode: bool) -> bool {
    if race.is_settled || is_race_preview_open(now, race) {
        return false;
    }
    if test_mode && race.total_runners == 0 {
        return false;
    }
    true
}

/// NFT entries accepted until `phase_2_close`.
pub fn is_entry_open(now: Timestamp, race: &RaceGlobal) -> bool {
    !race.is_settled && now < race.phase_2_close
}

/// Crowd salt commits — during prep (combined) or after entry closes (test stagger).
pub fn is_crowd_commit_open(now: Timestamp, race: &RaceGlobal, _test_mode: bool) -> bool {
    if race.is_settled || !crowd_entropy_enabled(race) {
        return false;
    }
    if combined_prep(race) {
        return is_entry_open(now, race);
    }
    now >= crowd_commit_start(race) && now < race.crowd_commit_close
}

/// Crowd salt reveals — after commit window, before reveal window ends.
pub fn is_crowd_reveal_open(now: Timestamp, race: &RaceGlobal, test_mode: bool) -> bool {
    if race.is_settled || !crowd_entropy_enabled(race) {
        return false;
    }
    if test_mode {
        return now >= race.crowd_commit_close && now < race.crowd_reveal_close;
    }
    now >= race.phase_3_open && now < race.crowd_reveal_close
}

/// Runner tactic reveal — same window as crowd reveal in test; production reveal phase.
pub fn is_reveal_open(now: Timestamp, race: &RaceGlobal, test_mode: bool) -> bool {
    if race.is_settled {
        return false;
    }
    if test_mode {
        return race.total_runners > 0 && is_crowd_reveal_open(now, race, true);
    }
    now >= race.phase_3_open && now < race.crowd_reveal_close
}

/// Settlement after all reveal windows close.
pub fn is_settlement_open(now: Timestamp, race: &RaceGlobal) -> bool {
    !race.is_settled && now >= race.phase_3_close
}

/// Live race preview — after reveals close, before settlement opens.
pub fn is_race_preview_open(now: Timestamp, race: &RaceGlobal) -> bool {
    if race.is_settled {
        return false;
    }
    now >= race.crowd_reveal_close && now < race.phase_3_close
}

/// Primary phase label for queries / UI.
pub fn current_phase(now: Timestamp, race: &RaceGlobal) -> RacePhase {
    if race.is_settled {
        return RacePhase::Settled;
    }
    if now < race.phase_2_close {
        return RacePhase::Entry;
    }
    if crowd_entropy_enabled(race)
        && !combined_prep(race)
        && now < race.crowd_commit_close
    {
        return RacePhase::CrowdCommit;
    }
    if crowd_entropy_enabled(race) && now < race.crowd_reveal_close {
        return RacePhase::CrowdReveal;
    }
    RacePhase::Reveal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RaceGlobal;
    use cosmwasm_std::Uint128;

    fn race_from_schedule(schedule: PhaseTimestamps) -> RaceGlobal {
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
        race
    }

    #[test]
    fn test_mode_phases_stagger_crowd_windows() {
        let anchor = Timestamp::from_seconds(1_000_000);
        let schedule = compute_test_phases(anchor);
        let base = anchor.seconds();
        assert_eq!(schedule.phase_2_close.seconds(), base + TEST_PHASE_LEN);
        assert_eq!(schedule.crowd_commit_close.seconds(), base + 2 * TEST_PHASE_LEN);
        assert_eq!(schedule.crowd_reveal_close.seconds(), base + TEST_CYCLE_SECS);
        assert_eq!(
            schedule.phase_3_close.seconds(),
            base + TEST_CYCLE_SECS + TEST_PREVIEW_LIVE_SECS
        );
    }

    #[test]
    fn test_mode_waiting_phases_keep_entry_open() {
        let now = Timestamp::from_seconds(1_000_000);
        let race = race_from_schedule(compute_test_phases_waiting());
        assert!(is_entry_open(now, &race));
        assert!(!is_betting_open(now, &race, true));
        assert_eq!(current_phase(now, &race), RacePhase::Entry);
    }

    #[test]
    fn test_mode_betting_opens_after_first_runner() {
        let now = Timestamp::from_seconds(1_000_000);
        let mut race = race_from_schedule(compute_test_phases(now));
        race.total_runners = 1;
        assert!(is_betting_open(now, &race, true));
        assert!(is_entry_open(now, &race));
    }

    #[test]
    fn test_mode_betting_stays_open_through_reveals() {
        let anchor = Timestamp::from_seconds(1_000_000);
        let mut race = race_from_schedule(compute_test_phases(anchor));
        race.total_runners = 1;
        let during_commit = ts(anchor.seconds() + TEST_PHASE_LEN + 60);
        let during_reveal = ts(anchor.seconds() + 2 * TEST_PHASE_LEN + 60);
        assert!(is_betting_open(during_commit, &race, true));
        assert!(is_betting_open(during_reveal, &race, true));
        assert!(!is_betting_open(
            ts(anchor.seconds() + TEST_CYCLE_SECS),
            &race,
            true
        ));
    }

    #[test]
    fn test_mode_crowd_commit_after_entry_close() {
        let anchor = Timestamp::from_seconds(1_000_000);
        let race = race_from_schedule(compute_test_phases(anchor));
        let during_entry = ts(anchor.seconds() + 60);
        let after_entry = ts(anchor.seconds() + TEST_PHASE_LEN + 60);
        let during_commit = ts(anchor.seconds() + 2 * TEST_PHASE_LEN + 60);
        assert!(!is_crowd_commit_open(during_entry, &race, true));
        assert!(is_crowd_commit_open(after_entry, &race, true));
        assert!(!is_crowd_commit_open(during_commit, &race, true));
        assert!(!is_crowd_reveal_open(after_entry, &race, true));
    }

    #[test]
    fn test_mode_preview_window_before_settlement() {
        let anchor = Timestamp::from_seconds(1_000_000);
        let mut race = race_from_schedule(compute_test_phases(anchor));
        race.total_runners = 1;
        let live_start = ts(anchor.seconds() + TEST_CYCLE_SECS);
        let during_live = ts(anchor.seconds() + TEST_CYCLE_SECS + 120);
        let before_settle = ts(anchor.seconds() + TEST_CYCLE_SECS + TEST_PREVIEW_LIVE_SECS - 30);
        let at_settle = ts(anchor.seconds() + TEST_CYCLE_SECS + TEST_PREVIEW_LIVE_SECS);
        assert!(!is_race_preview_open(
            ts(anchor.seconds() + TEST_CYCLE_SECS - 30),
            &race
        ));
        assert!(is_race_preview_open(live_start, &race));
        assert!(is_race_preview_open(during_live, &race));
        assert!(is_race_preview_open(before_settle, &race));
        assert!(!is_race_preview_open(at_settle, &race));
        assert!(!is_settlement_open(before_settle, &race));
        assert!(is_settlement_open(at_settle, &race));
    }

    #[test]
    fn test_mode_crowd_reveal_before_settlement() {
        let anchor = Timestamp::from_seconds(1_000_000);
        let mut race = race_from_schedule(compute_test_phases(anchor));
        race.total_runners = 1;
        let during_reveal = ts(anchor.seconds() + 2 * TEST_PHASE_LEN + 60);
        let before_settle = ts(anchor.seconds() + TEST_CYCLE_SECS - 30);
        assert!(is_crowd_reveal_open(during_reveal, &race, true));
        assert!(is_reveal_open(during_reveal, &race, true));
        assert!(!is_settlement_open(before_settle, &race));
        assert!(is_settlement_open(ts(anchor.seconds() + TEST_CYCLE_SECS + TEST_PREVIEW_LIVE_SECS), &race));
    }

    #[test]
    fn production_tri_daily_block_schedule() {
        let day_start = 1_700_000_000 - (1_700_000_000 % DAY_SECS);
        let now = ts(day_start + ONE_HOUR);
        let schedule = compute_production_phases(now);
        assert_eq!(schedule.phase_2_close.seconds(), day_start + PROD_PREP_SECS);
        assert_eq!(
            schedule.crowd_commit_close.seconds(),
            day_start + PROD_PREP_SECS
        );
        assert_eq!(
            schedule.crowd_reveal_close.seconds(),
            day_start + PROD_PREP_SECS + PROD_REVEAL_GRACE_SECS
        );
        assert_eq!(schedule.phase_3_close.seconds(), day_start + PROD_BLOCK_SECS);

        let race = race_from_schedule(schedule);
        assert!(is_entry_open(now, &race));
        assert!(is_crowd_commit_open(now, &race, false));
        assert!(is_betting_open(now, &race, false));
        assert_eq!(current_phase(now, &race), RacePhase::Entry);
        assert!(crowd_entropy_enabled(&race));
        assert!(combined_prep(&race));
    }

    #[test]
    fn production_new_race_skips_to_next_block_after_prep() {
        let day_start = 1_700_000_000 - (1_700_000_000 % DAY_SECS);
        let now_secs = day_start + PROD_PREP_SECS + ONE_HOUR;
        assert_eq!(
            production_block_start_for_new_race(now_secs),
            day_start + PROD_BLOCK_SECS
        );
    }

    #[test]
    fn production_combined_prep_crowd_commit_during_entry() {
        let day_start = 1_700_000_000 - (1_700_000_000 % DAY_SECS);
        let schedule = compute_production_phases_from_block(day_start);
        let race = race_from_schedule(schedule);
        let during_prep = ts(day_start + ONE_HOUR);
        assert!(is_entry_open(during_prep, &race));
        assert!(is_crowd_commit_open(during_prep, &race, false));
        let after_prep = ts(day_start + PROD_PREP_SECS + 60);
        assert!(!is_entry_open(after_prep, &race));
        assert!(!is_crowd_commit_open(after_prep, &race, false));
        assert!(is_crowd_reveal_open(after_prep, &race, false));
    }

    #[test]
    fn user_reveal_delay_blocks_early_reveal() {
        let committed = ts(1_000);
        assert!(!user_reveal_allowed(ts(1_000 + REVEAL_DELAY_SECS - 1), committed));
        assert!(user_reveal_allowed(ts(1_000 + REVEAL_DELAY_SECS), committed));
        assert!(user_reveal_allowed(ts(1_000 + REVEAL_DELAY_SECS + 100), committed));
    }

    #[test]
    fn production_live_window_is_final_two_hours_in_eight_hour_block() {
        let day_start = 1_700_000_000 - (1_700_000_000 % DAY_SECS);
        let schedule = compute_production_phases_from_block(day_start);
        let live_start = schedule.crowd_reveal_close.seconds();
        let settle = schedule.phase_3_close.seconds();
        // 8h block: 3h prep + 3h reveal → 2h live before settlement opens at block end.
        assert_eq!(settle - live_start, 2 * ONE_HOUR);
        assert_eq!(settle - day_start, PROD_BLOCK_SECS);
    }

    #[test]
    fn settlement_locked_until_phase_3_close() {
        let race = race_from_schedule(PhaseTimestamps {
            phase_1_close: ts(100),
            phase_2_close: ts(200),
            phase_3_open: ts(200),
            phase_3_close: ts(400),
            crowd_commit_close: ts(200),
            crowd_reveal_close: ts(400),
        });
        assert!(!is_settlement_open(ts(200), &race));
        assert!(is_reveal_open(ts(200), &race, false));
        assert!(is_settlement_open(ts(400), &race));
        assert!(!is_reveal_open(ts(400), &race, false));
    }
}
