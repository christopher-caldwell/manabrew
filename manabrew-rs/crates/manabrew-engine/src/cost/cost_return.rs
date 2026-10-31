//! Return permanents to hand as a cost. Mirrors Java's `CostReturn`.

use forge_foundation::ZoneType;

use crate::game::GameState;
use crate::ids::CardId;

pub fn pay_as_decided_self(game: &mut GameState, source: CardId) -> bool {
    let owner = game.card(source).owner;
    game.move_card(source, ZoneType::Hand, owner);
    true
}

pub fn pay_as_decided_cards(game: &mut GameState, cards: &[CardId]) -> bool {
    for &cid in cards {
        let owner = game.card(cid).owner;
        game.move_card(cid, ZoneType::Hand, owner);
    }
    true
}

pub const HASH_LKI: &str = "Returned";
pub const HASH_CARDS: &str = "ReturnedCards";

pub fn payment_order(part: &super::CostPart) -> i32 {
    part.payment_order()
}

pub fn eligible_returns(game: &GameState, player: crate::ids::PlayerId, filter: &str) -> Vec<CardId> {
    if filter.eq_ignore_ascii_case("Creature.attacking+unblocked") {
        use forge_foundation::PhaseType;
        if game.turn.active_player != player || !matches!(game.turn.phase, PhaseType::CombatDeclareBlockers | PhaseType::CombatFirstStrikeDamage | PhaseType::CombatDamage | PhaseType::CombatEnd) || !game.turn.combat_blockers_declared {
            return Vec::new();
        }
        return game.cards_in_zone(ZoneType::Battlefield, player).iter().copied().filter(|&cid| {
            let c = game.card(cid);
            c.controller == player && c.is_creature() && !c.phased_out && c.attacking_player.is_some() && !c.damage_history.creature_got_blocked_this_combat
        }).collect();
    }
    super::get_sacrifice_targets(game, player, filter)
}

pub fn can_pay(
    game: &crate::game::GameState,
    _available_mana: &crate::mana::ManaPool,
    source: crate::ids::CardId,
    player: crate::ids::PlayerId,
    _ability: Option<&crate::spellability::SpellAbility>,
    part: &super::CostPart,
) -> bool {
    let super::CostPart::Return {
        amount,
        type_filter,
    } = part
    else {
        return false;
    };
    if type_filter == "CARDNAME" {
        return game.card(source).zone == ZoneType::Battlefield;
    }
    let targets = eligible_returns(game, player, type_filter);
    (targets.len() as i32) >= amount.resolve(game, source, player)
}

pub fn pay_with_decision(
    game: &mut GameState,
    _player: crate::ids::PlayerId,
    source: CardId,
    part: &super::CostPart,
    decision: &crate::cost::payment_decision::PaymentDecision,
) -> bool {
    let super::CostPart::Return { type_filter, .. } = part else {
        return false;
    };
    if type_filter == "CARDNAME" || type_filter == "NICKNAME" {
        return pay_as_decided_self(game, source);
    }
    if let crate::cost::payment_decision::PaymentDecision::Cards(cards) = decision {
        return pay_as_decided_cards(game, cards);
    }
    false
}
