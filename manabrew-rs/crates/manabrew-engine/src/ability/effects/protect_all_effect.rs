use forge_foundation::ZoneType;

use super::{matches_valid_cards_for_sa, EffectContext};
use crate::ids::CardId;

/// End-of-turn revert for ProtectionAll. Mirrors the `GameCommand.run()` in Java
/// `ProtectAllEffect` that removes the granted protection keywords when the
/// effect duration expires.
///
/// Removes the specified protection keyword from the card's pump_keywords.
pub fn run(game: &mut crate::game::GameState, card_id: crate::ids::CardId, keyword: &str) {
    if game.card(card_id).zone == ZoneType::Battlefield {
        game.card_mut(card_id).pump_keywords.remove(keyword);
    }
}

/// `SP$ ProtectionAll` — grant protection to all matching permanents.
///
/// Mirrors Java's `ProtectAllEffect.java`.
/// - `ValidCards$` — filter for which permanents gain protection.
/// - `Gains$` — the protection keyword to grant.
/// - `Choices$` — if present, player chooses the protection quality.
///
/// # Card script examples
/// ```text
/// A:SP$ ProtectionAll | ValidCards$ Creature.YouCtrl | Gains$ Protection from chosen color
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `ProtectAllEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(ProtectAllEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let valid_filter = sa
        .ir
        .valid_cards_text
        .as_deref()
        .unwrap_or("Creature.YouCtrl")
        .to_string();
    let valid_selector = sa.ir.valid_cards_selector.as_ref();
    let Some(prot_keyword) = super::protect_effect::choose_protection_keyword(ctx, sa) else {
        return;
    };

    if prot_keyword.is_empty() {
        return;
    }

    let player_ids = ctx.game.player_order.clone();
    let mut targets: Vec<CardId> = Vec::new();

    for &pid in &player_ids {
        let zone_cards = ctx.game.cards_in_zone(ZoneType::Battlefield, pid).to_vec();
        for cid in zone_cards {
            if matches_valid_cards_for_sa(
                ctx.game,
                sa,
                ctx.game.card(cid),
                valid_selector,
                &valid_filter,
            ) {
                targets.push(cid);
            }
        }
    }

    for cid in targets {
        if ctx.game.card(cid).zone == ZoneType::Battlefield {
            ctx.game.card_mut(cid).pump_keywords.add(&prot_keyword);
        }
    }
}
