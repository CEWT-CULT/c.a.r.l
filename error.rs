use cosmwasm_std::StdError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Unauthorized")]
    Unauthorized {},

    #[error("Invalid Amount")]
    InvalidAmount {},

    #[error("Insufficient Funds")]
    InsufficientFunds {},

    #[error("Invalid Denom Sent")]
    InvalidDenomSent {},

    #[error("Wrong Phase")]
    WrongPhase {},

    #[error("Race Full")]
    RaceFull {},

    #[error("Already Entered")]
    AlreadyEntered {},

    #[error("Not Entered")]
    NotEntered {},

    #[error("Already Bet")]
    AlreadyBet {},

    #[error("Invalid NFT Contract")]
    InvalidNftContract {},

    #[error("Invalid Commitment")]
    InvalidCommitment {},

    #[error("Already Revealed")]
    AlreadyRevealed {},

    #[error("Already Settled")]
    AlreadySettled {},

    #[error("Not Settled")]
    NotSettled {},

    #[error("Already Claimed")]
    AlreadyClaimed {},

    #[error("Not Claimable")]
    NotClaimable {},

    #[error("Reveal Window Closed")]
    RevealWindowClosed {},

    #[error("Not crowd committed")]
    NotCrowdCommitted {},

    #[error("Already crowd committed")]
    AlreadyCrowdCommitted {},

    #[error("Crowd entropy cap reached")]
    CrowdEntropyFull {},

    #[error("Settlement window not open yet")]
    SettlementWindowClosed {},

    #[error("No side bet for this race")]
    NoSideBet {},

    #[error("Wager already claimed")]
    WagerAlreadyClaimed {},

    #[error("No wager payout available")]
    NoWagerPayout {},

    #[error("Race is within the retention window and cannot be pruned")]
    RaceWithinRetention {},

    #[error("Race preview not open yet")]
    RacePreviewClosed {},

    #[error("Preview crank too soon — wait one minute between cranks")]
    PreviewCrankTooSoon {},

    #[error("Preview complete")]
    PreviewComplete {},

    #[error("Reveal delay not elapsed — wait five minutes after commit")]
    RevealDelayNotElapsed {},

    #[error("Invalid racer pick")]
    InvalidRacerPick {},
}
