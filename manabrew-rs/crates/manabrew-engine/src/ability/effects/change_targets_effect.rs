//! ChangeTargets effect — redirect a spell or ability's targets.
//!
//! Ported from Java's `ChangeTargetsEffect.java`.
//! Change the target(s) of target spell or ability.
//! In Java this introspects the MagicStack to find the targeted spell's
//! StackInstance and updates its TargetChoices. In our engine, the stack
//! stores SpellAbility instances with target_chosen fields.

use forge_foundation::ZoneType;

use super::EffectContext;
use crate::ability::ability_ir::DefinedRef;
use crate::ids::CardId;
use crate::parsing::keys;
use crate::spellability::SpellAbility;

/// Configure the spell ability during construction.
/// Mirrors Java `ChangeTargetsEffect.buildSpellAbility` — sets the target zone
/// to Stack so that the ability targets spells on the stack.
pub fn build_spell_ability(sa: &mut SpellAbility) {
    if sa.uses_targeting() {
        if let Some(ref mut tr) = sa.target_restrictions {
            tr.tgt_zone = vec![ZoneType::Stack];
        }
    }
}

/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `ChangeTargetsEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(ChangeTargetsEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let controller = sa.activating_player;

    // Stack targets are represented by stack-entry identity, not necessarily
    // by the underlying card. A defined magnet must change the targeted
    // ability itself and retain the new target's object timestamp.
    if let (Some(entry_id), Some(magnet)) = (sa.target_chosen.target_stack_entry, sa.ir.defined_magnet_text.as_deref()) {
        let new_target = match DefinedRef::parse(magnet) {
            DefinedRef::SelfCard => sa.source,
            DefinedRef::ParentTarget => sa.target_chosen.target_card,
            _ => sa.source.and_then(|id| ctx.game.card(id).remembered_cards.first().copied()),
        };
        if let (Some(new_target), Some(entry)) = (new_target, ctx.game.stack.find_by_id(entry_id).cloned()) {
            let target_sa = &entry.spell_ability;
            let old_target = target_sa.target_chosen.target_card;
            let legal = target_sa.target_restrictions.as_ref().is_some_and(|restrictions| {
                restrictions.tgt_zone.contains(&ctx.game.card(new_target).zone)
                && crate::spellability::target_restrictions::get_all_candidates_any_filtered_for_restrictions(
                    ctx.game, restrictions, target_sa.activating_player, target_sa.source,
                ).contains(&new_target)
            }) && crate::spellability::target_restrictions::can_be_targeted_by_sa(
                ctx.game, new_target, target_sa.activating_player, target_sa,
            );
            let player_target = target_sa.target_chosen.target_player.is_some()
                && target_sa.target_chosen.additional_target_players.is_empty()
                && old_target.is_none();
            if legal && (old_target.is_some_and(|old| old != new_target) || player_target) {
                let timestamp = ctx.game.card(new_target).zone_timestamp;
                if let Some(entry) = ctx.game.stack.iter_mut().find(|entry| entry.id == entry_id) {
                    if let Some(old_target) = old_target {
                        entry.update_target(old_target, new_target);
                    } else {
                        entry.spell_ability.target_chosen.target_player = None;
                        entry.spell_ability.target_chosen.target_card = Some(new_target);
                    }
                    entry.spell_ability.target_chosen.target_card_zone_timestamp = Some(timestamp);
                }
            }
        }
        return;
    }

    // Find the targeted spell on the stack
    let target_spell_card = match sa.target_chosen.target_card {
        Some(card_id) => card_id,
        None => return,
    };

    // Verify the target is on the stack
    if ctx.game.card(target_spell_card).zone != ZoneType::Stack {
        return;
    }

    // Handle RandomTarget mode: pick a random new legal target
    if sa.param_is_true(keys::RANDOM_TARGET) {
        // Find all creatures/permanents on battlefield as candidates
        let candidates: Vec<CardId> = ctx
            .game
            .cards
            .iter()
            .filter(|c| c.zone == ZoneType::Battlefield)
            .map(|c| c.id)
            .collect();

        if candidates.is_empty() {
            return;
        }

        // Pick random new target
        let idx = ctx.rng.next_int(candidates.len() as i32) as usize % candidates.len();
        let new_target = candidates[idx];

        // Update the spell's target on the stack
        // In our simplified stack model, we update the card's svar to track new target
        ctx.game
            .card_mut(target_spell_card)
            .set_s_var("RedirectedTarget", format!("{}", new_target.0));
        return;
    }

    // Handle DefinedMagnet mode: redirect to a specific permanent
    if let Some(magnet_def) = sa.ir.defined_magnet_text.as_deref() {
        let magnet_ref = DefinedRef::parse(magnet_def);
        let new_target = if matches!(magnet_ref, DefinedRef::SelfCard) {
            sa.source
        } else if matches!(magnet_ref, DefinedRef::ParentTarget) {
            sa.target_chosen.target_card
        } else {
            sa.source
                .and_then(|sid| ctx.game.card(sid).remembered_cards.first().copied())
        };

        if let Some(new_tgt) = new_target {
            ctx.game
                .card_mut(target_spell_card)
                .set_s_var("RedirectedTarget", format!("{}", new_tgt.0));
        }
        return;
    }

    // Default mode: let player choose new targets
    // In full implementation this would present the controller with legal target choices.
    // Auto-mode: agent chooses from battlefield permanents
    let candidates: Vec<CardId> = ctx
        .game
        .cards
        .iter()
        .filter(|c| c.zone == ZoneType::Battlefield && c.controller != controller)
        .map(|c| c.id)
        .collect();

    if candidates.is_empty() {
        return;
    }

    // Agent chooses new target
    ctx.agents[controller.index()].snapshot_state(ctx.game, ctx.mana_pools);
    if let Some(chosen) = ctx.agents[controller.index()].choose_single_card_for_zone_change(
        ctx.game,
        controller,
        &candidates,
        "Choose new target",
        false,
    ) {
        ctx.game
            .card_mut(target_spell_card)
            .set_s_var("RedirectedTarget", format!("{}", chosen.0));
    }
}
