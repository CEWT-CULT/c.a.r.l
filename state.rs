use cosmwasm_std::{Addr, Binary, Timestamp, Uint128};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct Config {
    pub admin: Addr,
    pub denom: String,
    pub chicken_nft_address: Addr,
    pub newt_nft_address: Addr,
    /// Extra newt CW721 collections (primary is `newt_nft_address`).
    #[serde(default)]
    pub newt_nft_addresses: Vec<Addr>,
    pub penguin_nft_address: Addr,
    pub fly_nft_address: Addr,
    pub frog_nft_address: Addr,
    pub bull_nft_address: Addr,
    pub fox_nft_address: Addr,
    /// All duck CW721 collections map to the same race species.
    pub duck_nft_addresses: Vec<Addr>,
    #[serde(default)]
    pub manta_nft_address: Option<Addr>,
    /// All shrimp CW721 collections map to the same race species.
    #[serde(default)]
    pub shrimp_nft_addresses: Vec<Addr>,
    #[serde(default)]
    pub sloth_nft_address: Option<Addr>,
    #[serde(default)]
    pub moth_nft_address: Option<Addr>,
    #[serde(default)]
    pub snail_nft_address: Option<Addr>,
    #[serde(default)]
    pub steer_nft_address: Option<Addr>,
    #[serde(default)]
    pub goat_nft_address: Option<Addr>,
    /// All kitty CW721 collections map to the same race species.
    #[serde(default)]
    pub kitty_nft_addresses: Vec<Addr>,
    pub entry_fee: Uint128,
    pub test_mode: bool,
}
pub const CONFIG: Item<Config> = Item::new("config");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct Users {
    pub deposits: Uint128,
    pub last_action: Option<Timestamp>,
}
pub const USERS: Map<Addr, Users> = Map::new("users");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub enum RaceAction {
    Saboteur,
    Cheerleader,
    Wildcard,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
pub enum Species {
    Chicken,
    Newt,
    Penguin,
    Fly,
    Frog,
    Bull,
    Fox,
    Duck,
    Manta,
    Shrimp,
    Sloth,
    Moth,
    Snail,
    Steer,
    Goat,
    Kitty,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct RaceEntry {
    pub player: Addr,
    pub nft_contract: Addr,
    pub nft_id: String,
    pub species: Species,
    pub commitment: Binary,
    pub revealed_action: Option<RaceAction>,
    /// Set after a valid `reveal_race`; used to verify the entry commitment only.
    pub revealed_salt: Option<String>,
    pub final_rank: Option<u32>,
    #[serde(default)]
    pub nft_claimed: bool,
    #[serde(default = "zero_timestamp")]
    pub committed_at: Timestamp,
}
pub const RACE_ENTRIES: Map<(u64, Addr), RaceEntry> = Map::new("race_entries");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub enum BetType {
    ChickenVictory,
    NewtVictory,
    PenguinVictory,
    FlyVictory,
    FrogVictory,
    BullVictory,
    FoxVictory,
    DuckVictory,
    MantaVictory,
    ShrimpVictory,
    SlothVictory,
    MothVictory,
    SnailVictory,
    SteerVictory,
    GoatVictory,
    KittyVictory,
    UnderdogWins,
    /// Pick must be set on `SideBet::pick` — that racer must finish 1st.
    RacerVictory,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct SideBet {
    pub bettor: Addr,
    pub bet_type: BetType,
    pub amount: Uint128,
    /// Racer wallet when `bet_type` is `RacerVictory`.
    #[serde(default)]
    pub pick: Option<Addr>,
    #[serde(default)]
    pub claimed: bool,
}
pub const SIDE_BETS: Map<(u64, Addr), SideBet> = Map::new("side_bets");

/// Spectator entropy — commit blind salt while entry/betting outcomes are unknown.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct CrowdEntropy {
    pub bettor: Addr,
    /// `sha256(salt)` committed during the crowd-commit window.
    pub commitment: Binary,
    pub revealed_salt: Option<String>,
    #[serde(default = "zero_timestamp")]
    pub committed_at: Timestamp,
}
pub const CROWD_ENTROPY: Map<(u64, Addr), CrowdEntropy> = Map::new("crowd_entropy");

pub fn zero_timestamp() -> Timestamp {
    Timestamp::from_seconds(0)
}

/// Stored at settlement — bettors pull payouts via `ClaimWager` (O(1) writes per claim).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct SideBetSettlement {
    pub winning_species: Option<Species>,
    pub underdog_wins: bool,
    /// Overall race winner when side bets settled.
    #[serde(default)]
    pub winning_racer: Option<Addr>,
    /// Every bettor on the desk picked the same outcome — full stake refunds on claim.
    pub all_bets_off: bool,
    /// Admin rain-out — full stake refunds on claim.
    #[serde(default)]
    pub rained_out: bool,
    pub loser_contribution: Uint128,
    pub total_winning_wagers: Uint128,
    pub house_cut: Uint128,
    /// Rounding dust from pro-rata bonus floors; paid to `remainder_recipient` on claim.
    pub remainder: Uint128,
    pub remainder_recipient: Option<Addr>,
    /// Withheld runner SET or crowd salt — side wager forfeited (anti seed-reroll).
    #[serde(default)]
    pub slashed_bettors: Vec<Addr>,
}
pub const RACE_SIDE_BET_SETTLEMENT: Map<u64, SideBetSettlement> =
    Map::new("race_side_bet_settlement");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct UserProfile {
    pub level: u32,
    pub xp: u64,
    pub speed_talents: u32,
    pub stamina_talents: u32,
}
pub const USER_PROFILES: Map<Addr, UserProfile> = Map::new("user_profiles");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct RaceGlobal {
    pub current_race_id: u64,
    pub total_runners: u32,
    pub total_entry_pool: Uint128,
    pub total_bet_pool: Uint128,
    pub phase_1_close: Timestamp,
    pub phase_2_close: Timestamp,
    pub phase_3_open: Timestamp,
    /// Settlement unlocks after the reveal window closes.
    pub phase_3_close: Timestamp,
    /// Crowd salt commits accepted until this time.
    #[serde(default = "zero_timestamp")]
    pub crowd_commit_close: Timestamp,
    /// Crowd + runner reveals accepted until this time.
    #[serde(default = "zero_timestamp")]
    pub crowd_reveal_close: Timestamp,
    #[serde(default)]
    pub crowd_commit_count: u32,
    pub is_settled: bool,
    /// Public preview cranks after reveals close (0 = everyone on start line).
    #[serde(default)]
    pub preview_step: u8,
    #[serde(default = "zero_timestamp")]
    pub last_preview_crank: Timestamp,
}
pub const RACE_GLOBAL: Item<RaceGlobal> = Item::new("race_global");
/// Next race accepting entries/bets while `RACE_GLOBAL` is in reveal/live/settle.
pub const ENROLLING_RACE: Item<Option<RaceGlobal>> = Item::new("enrolling_race");

/// Full physics simulation cached on first preview crank (outcome fixed at reveal close).
pub const RACE_PREVIEW_SIM: Map<(u64, Addr), RaceResult> = Map::new("race_preview_sim");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct RaceResult {
    pub tick_distances: [u128; 5],
    pub total_distance: u128,
}
pub const RACE_RESULTS: Map<(u64, Addr), RaceResult> = Map::new("race_results");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct RaceHistoryEntry {
    pub race_id: u64,
    pub total_runners: u32,
    pub total_entry_pool: Uint128,
    pub total_bet_pool: Uint128,
    pub settled_at: Timestamp,
    pub winner: Option<Addr>,
    pub phase_1_close: Timestamp,
    pub phase_2_close: Timestamp,
    pub phase_3_open: Timestamp,
    #[serde(default)]
    pub rained_out: bool,
}
pub const RACE_HISTORY: Map<u64, RaceHistoryEntry> = Map::new("race_history");

pub fn default_user_profile() -> UserProfile {
    UserProfile {
        level: 1,
        xp: 0,
        speed_talents: 0,
        stamina_talents: 0,
    }
}

pub fn default_users() -> Users {
    Users {
        deposits: Uint128::zero(),
        last_action: None,
    }
}
