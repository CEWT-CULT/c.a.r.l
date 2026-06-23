pub mod contract;
mod error;
pub mod msg;
pub mod state;

mod crowd;
mod claim;
mod betting;
mod phases;
mod query;
mod race;
mod race_history;
mod receive_nft;
mod preview;
mod settlement;
mod slots;
mod species;
mod vault;

pub use crate::error::ContractError;
pub use crate::species::{
    bet_type_for_species, species_for_bet_type, species_from_contract, species_label, ALL_SPECIES,
};
