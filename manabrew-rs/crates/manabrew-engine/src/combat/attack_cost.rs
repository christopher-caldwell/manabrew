//! Attack cost computation (Propaganda, Ghostly Prison, etc.).
//!
//! Mirrors Java Forge's `CombatUtil.getAttackCost()` — scans battlefield for
//! `CantAttackUnless` static abilities and accumulates the mana cost an
//! attacker must pay to attack a given defender.

use forge_foundation::ZoneType;

use crate::card::{valid_filter, Card};
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
pub fn get_attack_cost(cards: &[Card], attacker: &Card, defender: DefenderId) -> i32 {
    let mut total_cost = 0;

    for source in cards.iter().filter(|c| c.zone == ZoneType::Battlefield) {
        for sa in &source.static_abilities {
            if !sa.check_mode(&StaticMode::CantAttackUnless) {
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

            // Check Target$ — who is being defended
            if let Some(target_param) = sa.ir.target_text.as_deref() {
                match target_param {
                    "You" => {
                        // Only applies when attacking the source's controller
                        let defends_controller = match defender {
                            DefenderId::Player(pid) => pid == source.controller,
                            DefenderId::Permanent(cid) => {
                                cards[cid.index()].controller == source.controller
                            }
                        };
                        if !defends_controller {
                            continue;
                        }
                    }
                    _ => {
                        // Applies to any attack
                    }
                }
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
        assert_eq!(get_attack_cost(&game.cards, game.card(a), DefenderId::Player(PlayerId(1))), 2);
        assert_eq!(get_attack_cost(&game.cards, game.card(a), DefenderId::Player(PlayerId(2))), 0);
    }
}
