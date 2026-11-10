//! Attack cost computation (Propaganda, Ghostly Prison, etc.).
//!
//! Mirrors Java Forge's `CombatUtil.getAttackCost()` — scans battlefield for
//! `CantAttackUnless` static abilities and accumulates the mana cost an
//! attacker must pay to attack a given defender.

use forge_foundation::ZoneType;

use crate::card::{valid_filter, Card};
use crate::game::GameState;
use crate::combat::DefenderId;
use crate::staticability::StaticMode;

/// Compute the total generic mana cost required for `attacker` to attack `defender`.
///
/// Returns the accumulated cost as a generic mana amount, or 0 if no cost.
/// Scans all battlefield permanents for `Mode$ CantAttackUnless` statics.
///
/// Card script example (Propaganda):
/// ```text
/// S:Mode$ CantAttackUnless | ValidCard$ Creature | Target$ You | Cost$ 2
/// ```
pub fn get_attack_cost(game: &GameState, attacker: &Card, defender: DefenderId) -> i32 {
    let mut total_cost = 0;

    for source in game.cards.iter().filter(|c| c.zone == ZoneType::Battlefield) {
        for sa in &source.static_abilities {
            if !sa.check_conditions_full(&StaticMode::CantAttackUnless, source, game) {
                continue;
            }

            // Check ValidCard$ matches the attacker
            if !valid_filter::matches_valid_card_selector_opt(
                sa.ir.valid_card.as_ref(),
                attacker,
                source,
            ) {
                continue;
            }

            // A target selector may contain both player and permanent alternatives.
            // Match each against the actual defender, not its controller: otherwise
            // a permanent-only alternative can erroneously match any player.
            let target_matches = sa.ir.target_text.as_deref().map_or(true, |target| {
                target.split(',').any(|part| match defender {
                    DefenderId::Player(pid) => valid_filter::matches_valid(part.trim(), None, Some(pid), source, source.controller),
                    DefenderId::Permanent(cid) => valid_filter::matches_valid(part.trim(), Some(game.card(cid)), None, source, source.controller),
                })
            });
            if !target_matches {
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
mod scoped_tax_regression {
    use super::*;
    use crate::game::GameState;
    use crate::ids::PlayerId;
    use forge_carddb::parse_card_script;

    #[test]
    fn tax_is_charged_only_for_its_controller_as_defender() {
        let mut game = GameState::new(&["A", "B", "C"], 40);
        let creature = parse_card_script("Name:Attacker\nTypes:Creature Human\nPT:2/2").unwrap();
        let tax = parse_card_script("Name:Tax\nTypes:Enchantment\nS:Mode$ CantAttackUnless | ValidCard$ Creature | Target$ You | Cost$ 2").unwrap();
        let a = game.create_card(Card::from_rules(&creature, PlayerId(0)));
        let t = game.create_card(Card::from_rules(&tax, PlayerId(1)));
        game.move_card(a, ZoneType::Battlefield, PlayerId(0));
        game.move_card(t, ZoneType::Battlefield, PlayerId(1));
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(1))), 2);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(2))), 0);
    }
    #[test]
    fn player_qualified_target_is_relative_to_tax_source() {
        let mut game = GameState::new(&["A", "B", "C"], 40);
        let creature = parse_card_script("Name:Attacker\nTypes:Creature Human\nPT:2/2").unwrap();
        let tax = parse_card_script("Name:Tax\nTypes:Enchantment\nS:Mode$ CantAttackUnless | ValidCard$ Creature | Target$ Player.You | Cost$ 1").unwrap();
        let a = game.create_card(Card::from_rules(&creature, PlayerId(0)));
        let t = game.create_card(Card::from_rules(&tax, PlayerId(1)));
        game.move_card(a, ZoneType::Battlefield, PlayerId(0));
        game.move_card(t, ZoneType::Battlefield, PlayerId(1));
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(1))), 1);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(2))), 0);
    }

    #[test]
    fn mixed_player_and_planeswalker_targets_keep_their_scope() {
        let mut game = GameState::new(&["A", "B", "C"], 40);
        let creature = parse_card_script("Name:Attacker\nTypes:Creature Human\nPT:2/2").unwrap();
        let tax = parse_card_script("Name:Tax\nTypes:Creature Angel\nPT:3/5\nS:Mode$ CantAttackUnless | IsPresent$ Card.Self+untapped | ValidCard$ Creature | Target$ You,Planeswalker.YouCtrl | Cost$ 1").unwrap();
        let walker = parse_card_script("Name:Walker\nTypes:Planeswalker\nLoyalty:3").unwrap();
        let a = game.create_card(Card::from_rules(&creature, PlayerId(1)));
        let t = game.create_card(Card::from_rules(&tax, PlayerId(0)));
        let w = game.create_card(Card::from_rules(&walker, PlayerId(0)));
        let other = game.create_card(Card::from_rules(&walker, PlayerId(2)));
        for (id, owner) in [(a, PlayerId(1)), (t, PlayerId(0)), (w, PlayerId(0)), (other, PlayerId(2))] { game.move_card(id, ZoneType::Battlefield, owner); }
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(0))), 1);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(2))), 0);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Permanent(w)), 1);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Permanent(other)), 0);
        game.tap(t);
        assert_eq!(get_attack_cost(&game, game.card(a), DefenderId::Player(PlayerId(0))), 0);
    }

}
