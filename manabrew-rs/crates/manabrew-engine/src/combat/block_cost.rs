//! Block cost computation (War Cadence, etc.).
//!
//! Mirrors Java Forge's `CombatUtil.getBlockCost()` — scans battlefield for
//! `CantBlockUnless` static abilities and accumulates the mana cost a
//! blocker must pay to block a given attacker.

use forge_foundation::ZoneType;

use crate::card::{valid_filter, Card};
use crate::game::GameState;
use crate::staticability::StaticMode;

/// Compute the total generic mana cost required for `blocker` to block `attacker`.
///
/// Returns the accumulated cost as a generic mana amount, or 0 if no cost.
///
/// Card script example (War Cadence):
/// ```text
/// S:Mode$ CantBlockUnless | ValidCard$ Creature | Cost$ 1
/// ```
pub fn get_block_cost(game: &GameState, blocker: &Card, attacker: &Card) -> i32 {
    let mut total_cost = 0;

    for source in game.cards.iter().filter(|c| c.zone == ZoneType::Battlefield) {
        for sa in &source.static_abilities {
            if !sa.check_conditions_full(&StaticMode::CantBlockUnless, source, game) {
                continue;
            }

            // Check ValidCard$ matches the blocker
            if !valid_filter::matches_valid_card_selector_opt(
                sa.ir.valid_card.as_ref(),
                blocker,
                source,
            ) {
                continue;
            }

            if !valid_filter::matches_valid_player_opt(
                sa.ir.attacker_text.as_deref(),
                attacker.controller,
                source.controller,
            ) {
                continue;
            }

            // Parse Cost$ parameter as generic mana amount
            if let Some(cost_str) = sa.ir.cost.as_deref() {
                if let Ok(cost) = cost_str.trim().parse::<i32>() {
                    total_cost += cost;
                }
            }
        }
    }

    total_cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::GameState;
    use crate::ids::PlayerId;
    use forge_carddb::parse_card_script;

    #[test]
    fn block_tax_is_scoped_to_attacker_controller() {
        let mut game = GameState::new(&["A", "B", "C"], 40);
        let creature = parse_card_script("Name:Creature\nTypes:Creature Human\nPT:2/2").unwrap();
        let tax = parse_card_script("Name:Tax\nTypes:Enchantment\nS:Mode$ CantBlockUnless | ValidCard$ Creature | Attacker$ You | Cost$ 1").unwrap();
        let attacker = game.create_card(Card::from_rules(&creature, PlayerId(0)));
        let blocker = game.create_card(Card::from_rules(&creature, PlayerId(1)));
        let source = game.create_card(Card::from_rules(&tax, PlayerId(2)));
        for (id, controller) in [(attacker, PlayerId(0)), (blocker, PlayerId(1)), (source, PlayerId(2))] {
            game.move_card(id, ZoneType::Battlefield, controller);
        }
        assert_eq!(get_block_cost(&game, game.card(blocker), game.card(attacker)), 0);
        game.card_mut(attacker).set_controller(PlayerId(2));
        assert_eq!(get_block_cost(&game, game.card(blocker), game.card(attacker)), 1);
    }
    #[test]
    fn conditional_block_tax_only_applies_on_sources_turn() {
        let mut game = GameState::new(&["A", "B"], 40);
        let creature = parse_card_script("Name:Creature\nTypes:Creature Human\nPT:2/2").unwrap();
        let tax = parse_card_script("Name:Tax\nTypes:Enchantment\nS:Mode$ CantBlockUnless | ValidCard$ Creature | PlayerTurn$ You | Cost$ 1").unwrap();
        let attacker = game.create_card(Card::from_rules(&creature, PlayerId(0)));
        let blocker = game.create_card(Card::from_rules(&creature, PlayerId(1)));
        let source = game.create_card(Card::from_rules(&tax, PlayerId(1)));
        for (id, controller) in [(attacker, PlayerId(0)), (blocker, PlayerId(1)), (source, PlayerId(1))] {
            game.move_card(id, ZoneType::Battlefield, controller);
        }
        game.turn.active_player = PlayerId(0);
        assert_eq!(get_block_cost(&game, game.card(blocker), game.card(attacker)), 0);
        game.turn.active_player = PlayerId(1);
        assert_eq!(get_block_cost(&game, game.card(blocker), game.card(attacker)), 1);
    }

}
