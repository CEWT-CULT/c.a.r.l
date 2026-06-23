use crate::state::{BetType, RaceAction, Species, UserProfile};
use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Binary, Timestamp, Uint128};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct MigrateMsg {}

#[cw_serde]
pub struct InstantiateMsg {
    pub chicken_nft_address: String,
    pub newt_nft_address: String,
    pub newt_nft_addresses: Vec<String>,
    pub penguin_nft_address: String,
    pub fly_nft_address: String,
    pub frog_nft_address: String,
    pub bull_nft_address: String,
    pub fox_nft_address: String,
    pub duck_nft_addresses: Vec<String>,
    pub manta_nft_address: String,
    pub shrimp_nft_addresses: Vec<String>,
    pub sloth_nft_address: String,
    pub moth_nft_address: String,
    pub snail_nft_address: String,
    pub steer_nft_address: String,
    pub goat_nft_address: String,
    pub kitty_nft_addresses: Vec<String>,
    pub denom: String,
    pub entry_fee: Uint128,
    pub test_mode: bool,
}

#[cw_serde]
pub struct Cw721ReceiveMsg {
    pub sender: Addr,
    pub token_id: String,
    pub msg: Binary,
}

#[cw_serde]
pub struct EnterRaceMsg {
    pub commitment: Binary,
}

#[cw_serde]
pub enum Cw721ExecuteMsg {
    TransferNft { recipient: String, token_id: String },
}

#[cw_serde]
pub enum ExecuteMsg {
    Deposit {},
    Withdraw { amount: Uint128 },
    ReceiveNft(Cw721ReceiveMsg),
    PlaceSideBet {
        bet_type: BetType,
        amount: Uint128,
        #[serde(default)]
        pick: Option<Addr>,
    },
    RevealRace { action: RaceAction, salt: String },
    /// Blind crowd salt commit — requires an active side bet for this race.
    CommitCrowdEntropy { commitment: Binary },
    /// Reveal crowd salt before settlement.
    RevealCrowdEntropy { salt: String },
    /// Permissionless after reveal window closes; operator earns 2% of total pool at settle.
    SettleRace {},
    /// Permissionless — advances live race preview one step per minute (cosmetic only).
    CrankRacePreview {},
    ClaimRacerNft { race_id: u64 },
    /// Pull-pattern side-bet payout after settlement (or rain-out refund).
    ClaimWager { race_id: u64 },
    /// Permissionless — opens the next race for entry once the running race prep closes.
    OpenNextRace {},
    /// Permissionless — rolls the game to the next race cycle.
    AdvanceRace {},
    AdminSetTestPhases {
        phase_1_close: Timestamp,
        phase_2_close: Timestamp,
        phase_3_open: Timestamp,
        phase_3_close: Timestamp,
        crowd_commit_close: Option<Timestamp>,
        crowd_reveal_close: Option<Timestamp>,
    },
    /// Admin emergency: refund all entry fees + side bets, mark NFTs claimable, close race.
    AdminRainOutRace {},
    /// Admin: update entry fee for future NFT entries.
    AdminSetEntryFee { entry_fee: Uint128 },
    /// Permissionless batched deletion of historical race data beyond the retention window.
    PruneHistory { race_id: u64, limit: Option<u32> },
}

#[cw_serde]
pub enum RacePhase {
    Entry,
    Betting,
    CrowdCommit,
    CrowdReveal,
    Reveal,
    Settled,
}

#[cw_serde]
pub struct UserResponse {
    pub deposits: Uint128,
    pub last_action: Option<Timestamp>,
    pub profile: UserProfile,
}

#[cw_serde]
pub struct RosterEntry {
    pub player: Addr,
    pub nft_contract: Addr,
    pub nft_id: String,
    pub species: Species,
    pub revealed_action: Option<RaceAction>,
    pub final_rank: Option<u32>,
    pub nft_claimed: bool,
}

#[cw_serde]
pub struct SideBetEntry {
    pub bettor: Addr,
    pub bet_type: BetType,
    pub amount: Uint128,
    #[serde(default)]
    pub pick: Option<Addr>,
}

#[cw_serde]
pub struct SideBetDeskResponse {
    pub bets: Vec<SideBetEntry>,
    pub distinct_bet_types: u32,
    pub one_sided: bool,
    pub total_pool: Uint128,
}

#[cw_serde]
pub struct RaceHistoryResponse {
    pub races: Vec<crate::state::RaceHistoryEntry>,
    pub next: Option<u64>,
    pub retention_limit: u64,
}

#[cw_serde]
pub struct PreviewRunner {
    pub player: Addr,
    pub species: Species,
    pub nft_contract: Addr,
    pub nft_id: String,
    pub cumulative: u128,
    pub preview_step: u8,
}

#[cw_serde]
pub struct TelemetryRunner {
    pub player: Addr,
    pub species: Species,
    pub tick_distances: [u128; 5],
    pub final_rank: Option<u32>,
}

#[cw_serde]
pub struct CrowdEntropyResponse {
    pub bettor: Addr,
    pub commitment: Binary,
    pub revealed: bool,
}

#[cw_serde]
pub struct CrowdEntropyDeskResponse {
    pub commits: u32,
    pub reveals: u32,
    pub max_commits: u32,
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(crate::state::Config)] Config {},
    #[returns(crate::state::RaceGlobal)] RaceGlobal {},
    #[returns(Option<crate::state::RaceGlobal>)] EnrollingRace {},
    #[returns(UserResponse)] User { addr: Addr },
    #[returns(crate::state::RaceEntry)] RaceEntry { race_id: u64, addr: Addr },
    #[returns(Vec<RosterEntry>)] RaceRoster { race_id: u64 },
    #[returns(crate::state::SideBet)] SideBet { race_id: u64, addr: Addr },
    #[returns(SideBetDeskResponse)] SideBetDesk { race_id: u64 },
    #[returns(RaceHistoryResponse)] RaceHistory {
        start_after: Option<u64>,
        limit: Option<u32>,
    },
    #[returns(Vec<TelemetryRunner>)] RaceTelemetry { race_id: u64 },
    #[returns(Vec<PreviewRunner>)] RacePreview { race_id: u64 },
    #[returns(RacePhase)] CurrentPhase {},
    #[returns(crate::state::SideBetSettlement)] SideBetSettlement { race_id: u64 },
    #[returns(CrowdEntropyResponse)] CrowdEntropy { race_id: u64, addr: Addr },
    #[returns(CrowdEntropyDeskResponse)] CrowdEntropyDesk { race_id: u64 },
}
