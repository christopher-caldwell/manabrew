//! An opt-in record of rules events as they happen, for observers such as simulation traces.
//!
//! Off by default. An observer calls [`GameState::enable_journal`], then reads
//! [`GameState::journal`] or takes it with [`GameState::drain_journal`]. Nothing in the rules reads it.
use crate::game::GameState;
use crate::ids::{CardId, PlayerId};
use forge_foundation::ZoneType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEvent {
    /// A card moved zones. `to` is where it ended up after replacement effects.
    ZoneChange { card: CardId, from: ZoneType, to: ZoneType, controller: PlayerId },
    /// Damage a player was dealt after prevention and replacement.
    DamageToPlayer { source: Option<CardId>, target: PlayerId, amount: i32, combat: bool },
    /// Damage a card was dealt after prevention and replacement.
    DamageToCard { source: Option<CardId>, target: CardId, amount: i32, combat: bool },
    /// Life gained, or lost other than by damage (paying life, drain effects).
    LifeGained { player: PlayerId, amount: i32 },
    LifeLost { player: PlayerId, amount: i32 },
    /// A life total was set directly; `change` is new minus old.
    LifeSet { player: PlayerId, change: i32 },
}

impl GameState {
    pub fn enable_journal(&mut self) {
        self.journal.get_or_insert_with(Vec::new);
    }

    /// Everything recorded since the journal was enabled, oldest first.
    pub fn journal(&self) -> &[JournalEvent] {
        self.journal.as_deref().unwrap_or_default()
    }

    pub fn drain_journal(&mut self) -> Vec<JournalEvent> {
        self.journal.as_mut().map(std::mem::take).unwrap_or_default()
    }

    pub(crate) fn journal_push(&mut self, event: JournalEvent) {
        if let Some(journal) = self.journal.as_mut() {
            journal.push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_is_off_until_enabled_and_records_life_and_moves() {
        let mut game = GameState::new(&["A", "B"], 40);
        let a = PlayerId(0);
        game.player_gain_life(a, 3);
        assert!(game.drain_journal().is_empty());
        game.enable_journal();
        game.player_gain_life(a, 2);
        game.player_lose_life(a, 1);
        game.deal_damage_to_player_from(PlayerId(1), 4, None, true);
        assert_eq!(
            game.drain_journal(),
            vec![
                JournalEvent::LifeGained { player: a, amount: 2 },
                JournalEvent::LifeLost { player: a, amount: 1 },
                JournalEvent::DamageToPlayer { source: None, target: PlayerId(1), amount: 4, combat: true },
            ]
        );
        assert!(game.drain_journal().is_empty());
    }
}
