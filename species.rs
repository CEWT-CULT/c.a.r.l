use crate::state::{BetType, Config, Species};
use cosmwasm_std::Addr;

pub const ALL_SPECIES: [Species; 16] = [
    Species::Chicken,
    Species::Newt,
    Species::Penguin,
    Species::Fly,
    Species::Frog,
    Species::Bull,
    Species::Fox,
    Species::Duck,
    Species::Manta,
    Species::Shrimp,
    Species::Sloth,
    Species::Moth,
    Species::Snail,
    Species::Steer,
    Species::Goat,
    Species::Kitty,
];

fn is_duck_contract(config: &Config, nft_contract: &Addr) -> bool {
    config.duck_nft_addresses.iter().any(|a| a == nft_contract)
}

fn is_newt_contract(config: &Config, nft_contract: &Addr) -> bool {
    nft_contract == &config.newt_nft_address
        || config
            .newt_nft_addresses
            .iter()
            .any(|a| a == nft_contract)
}

fn is_shrimp_contract(config: &Config, nft_contract: &Addr) -> bool {
    config.shrimp_nft_addresses.iter().any(|a| a == nft_contract)
}

fn is_kitty_contract(config: &Config, nft_contract: &Addr) -> bool {
    config.kitty_nft_addresses.iter().any(|a| a == nft_contract)
}

pub fn species_from_contract(config: &Config, nft_contract: &Addr) -> Option<Species> {
    if nft_contract == &config.chicken_nft_address {
        Some(Species::Chicken)
    } else if is_newt_contract(config, nft_contract) {
        Some(Species::Newt)
    } else if nft_contract == &config.penguin_nft_address {
        Some(Species::Penguin)
    } else if nft_contract == &config.fly_nft_address {
        Some(Species::Fly)
    } else if nft_contract == &config.frog_nft_address {
        Some(Species::Frog)
    } else if nft_contract == &config.bull_nft_address {
        Some(Species::Bull)
    } else if nft_contract == &config.fox_nft_address {
        Some(Species::Fox)
    } else if is_duck_contract(config, nft_contract) {
        Some(Species::Duck)
    } else if config
        .manta_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Manta)
    } else if is_shrimp_contract(config, nft_contract) {
        Some(Species::Shrimp)
    } else if config
        .sloth_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Sloth)
    } else if config
        .moth_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Moth)
    } else if config
        .snail_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Snail)
    } else if config
        .steer_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Steer)
    } else if config
        .goat_nft_address
        .as_ref()
        .is_some_and(|a| a == nft_contract)
    {
        Some(Species::Goat)
    } else if is_kitty_contract(config, nft_contract) {
        Some(Species::Kitty)
    } else {
        None
    }
}

pub fn bet_type_for_species(species: Species) -> BetType {
    match species {
        Species::Chicken => BetType::ChickenVictory,
        Species::Newt => BetType::NewtVictory,
        Species::Penguin => BetType::PenguinVictory,
        Species::Fly => BetType::FlyVictory,
        Species::Frog => BetType::FrogVictory,
        Species::Bull => BetType::BullVictory,
        Species::Fox => BetType::FoxVictory,
        Species::Duck => BetType::DuckVictory,
        Species::Manta => BetType::MantaVictory,
        Species::Shrimp => BetType::ShrimpVictory,
        Species::Sloth => BetType::SlothVictory,
        Species::Moth => BetType::MothVictory,
        Species::Snail => BetType::SnailVictory,
        Species::Steer => BetType::SteerVictory,
        Species::Goat => BetType::GoatVictory,
        Species::Kitty => BetType::KittyVictory,
    }
}

pub fn species_for_bet_type(bet_type: &BetType) -> Option<Species> {
    match bet_type {
        BetType::ChickenVictory => Some(Species::Chicken),
        BetType::NewtVictory => Some(Species::Newt),
        BetType::PenguinVictory => Some(Species::Penguin),
        BetType::FlyVictory => Some(Species::Fly),
        BetType::FrogVictory => Some(Species::Frog),
        BetType::BullVictory => Some(Species::Bull),
        BetType::FoxVictory => Some(Species::Fox),
        BetType::DuckVictory => Some(Species::Duck),
        BetType::MantaVictory => Some(Species::Manta),
        BetType::ShrimpVictory => Some(Species::Shrimp),
        BetType::SlothVictory => Some(Species::Sloth),
        BetType::MothVictory => Some(Species::Moth),
        BetType::SnailVictory => Some(Species::Snail),
        BetType::SteerVictory => Some(Species::Steer),
        BetType::GoatVictory => Some(Species::Goat),
        BetType::KittyVictory => Some(Species::Kitty),
        BetType::UnderdogWins | BetType::RacerVictory => None,
    }
}

pub fn species_label(species: Species) -> &'static str {
    match species {
        Species::Chicken => "chicken",
        Species::Newt => "newt",
        Species::Penguin => "penguin",
        Species::Fly => "fly",
        Species::Frog => "frog",
        Species::Bull => "bull",
        Species::Fox => "fox",
        Species::Duck => "duck",
        Species::Manta => "manta",
        Species::Shrimp => "shrimp",
        Species::Sloth => "sloth",
        Species::Moth => "moth",
        Species::Snail => "snail",
        Species::Steer => "steer",
        Species::Goat => "goat",
        Species::Kitty => "kitty",
    }
}
