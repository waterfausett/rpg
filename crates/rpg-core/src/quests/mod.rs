//! Timed quests: definitions (`def`) loaded from TOML files in `rules/quests/`.
pub mod def;

pub use def::{load_dir, parse_quest, QuestDef, QuestStep, Same};
