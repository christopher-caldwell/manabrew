//! Stack removal — bouncing/exiling spells on the stack.
//!
//! Mirrors Java's `removeFromStack` (lines 1593-1649).

use forge_foundation::ZoneType;

use super::super::{emit_zone_trigger, EffectContext};
use crate::ids::PlayerId;
use crate::spellability::SpellAbility;

/// Remove a spell from the stack and move it to a destination zone.
pub(super) fn resolve_stack_removal(
    ctx: &mut EffectContext,
    sa: &SpellAbility,
    dest_zone: ZoneType,
    lib_position: &str,
    _controller: PlayerId,
) {
    // A sub-ability may inherit its parent's stack target without acquiring
    // target restrictions of its own (e.g. Narset's Reversal).
    let stack_target = if sa.uses_targeting() || sa.defined().is_some_and(|d| d.eq_ignore_ascii_case("Targeted")) {
        sa.target_chosen.target_stack_entry
    } else { None };
    let target_card = if let Some(id) = stack_target {
        ctx.game.stack.iter().find(|entry| entry.id == id)
            .and_then(|entry| entry.spell_ability.source)
    } else if sa.uses_targeting() || sa.defined().is_some_and(|d| d.eq_ignore_ascii_case("Targeted")) {
        sa.target_chosen.target_card
    } else if let Some(src) = sa
        .trigger_source
        .filter(|&cid| ctx.game.card(cid).zone == ZoneType::Stack)
    {
        Some(src)
    } else {
        // Fallback to `Defined$ Self` (the source card) when the SA has no
        // targets and no triggering card. Java's stack-removal path defaults
        // to the host card here; without this, abilities like Avatar's
        // Wrath's `DB$ ChangeZone | Origin$ Stack | Destination$ Exile`
        // never exile the spell, leaving it to the default graveyard move.
        sa.source
            .filter(|&cid| ctx.game.card(cid).zone == ZoneType::Stack)
    };

    let Some(card_id) = target_card else { return };
    if ctx.game.card(card_id).zone != ZoneType::Stack {
        return;
    }

    // Tokens on stack cease to exist when exiled
    if dest_zone == ZoneType::Exile && ctx.game.card(card_id).is_token {
        return;
    }

    let old_zone = ctx.game.card(card_id).zone;
    let dest_owner = ctx.game.card(card_id).owner;
    ctx.move_card(card_id, dest_zone, dest_owner);
    if ctx.game.card(card_id).zone != ZoneType::Stack {
        if let Some(id) = stack_target {
            ctx.game.stack.remove_by_id(id);
        } else if let Some(id) = ctx.game.stack.find_by_source_card(card_id).map(|entry| entry.id) {
            ctx.game.stack.remove_by_id(id);
        }
    }

    // ExiledWith for exile
    if dest_zone == ZoneType::Exile {
        if let Some(source_id) = sa.source {
            if sa.ir.exiled_with_effect_source {
                let exile_source = ctx.game.card(source_id).effect_source.unwrap_or(source_id);
                ctx.game.card_mut(card_id).set_exiled_by(Some(exile_source));
                ctx.game.card_mut(exile_source).add_remembered_card(card_id);
            }
        }
    }

    // Library positioning
    if dest_zone == ZoneType::Library
        && (lib_position == "-1" || lib_position.eq_ignore_ascii_case("Bottom"))
    {
        ctx.game
            .reorder_card_in_zone(ZoneType::Library, dest_owner, card_id, 0);
    }

    if dest_zone == ZoneType::Library && sa.is_shuffle() {
        ctx.game
            .shuffle_zone_cards(ZoneType::Library, dest_owner, ctx.rng);
    }

    // Counters
    if let Some(ct) = sa.with_counters_type_enum() {
        let amount = crate::svar::resolve_numeric_svar(
            ctx.game,
            sa,
            crate::parsing::keys::WITH_COUNTERS_AMOUNT,
            1,
        );
        ctx.add_counter(card_id, ct, amount, sa, crate::event::RunParams::default());
    }

    // Remember/Imprint
    if sa.is_remember_changed() {
        if let Some(sid) = sa.source {
            ctx.game.card_mut(sid).add_remembered_card(card_id);
        }
    }
    if sa.is_imprint() {
        if let Some(sid) = sa.source {
            ctx.game.card_mut(sid).add_imprinted_card(card_id);
        }
    }

    emit_zone_trigger(ctx.trigger_handler, ctx.game, card_id, old_zone, dest_zone);
}

#[cfg(test)]
mod targeted_stack_bounce_regression {
    use crate::{agent::{PassAgent, PlayerAgent}, card::Card, game::GameState,
        game_loop::GameLoop, ids::{CardId, PlayerId}, spellability::build_spell_ability,
        zone::magic_stack::StackEntry};
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};

    fn push(game: &mut GameState, sa: crate::spellability::SpellAbility) -> u32 {
        game.stack.push(StackEntry { id: 0, spell_ability: sa, is_creature_spell: false,
            is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
            optional_trigger_decider: None, optional_trigger_description: None, optional_trigger_source_name: None })
    }
    #[test]
    fn inherited_stack_target_is_returned_and_decoy_remains() {
        let mut game = GameState::new(&["A", "B"], 40);
        let player = PlayerId(0);
        let mut ids = Vec::new();
        for name in ["Target", "Decoy", "Reversal"] {
            let id = game.create_card(Card::new(CardId(0), name.into(), player,
                CardTypeLine::parse("Instant"), ManaCost::parse(""), ColorSet::COLORLESS, None, None, vec![], vec![]));
            game.move_card(id, ZoneType::Stack, player); ids.push(id);
        }
        let target = build_spell_ability(&game, ids[0], "SP$ GainLife | Defined$ You | LifeAmount$ 1", player);
        let target_id = push(&mut game, target);
        let decoy = build_spell_ability(&game, ids[1], "SP$ Draw | Defined$ You | NumCards$ 1", player);
        let decoy_id = push(&mut game, decoy);
        let mut bounce = build_spell_ability(&game, ids[2], "DB$ ChangeZone | Origin$ Stack | Destination$ Hand | Defined$ Targeted", player);
        bounce.is_trigger = true; bounce.target_chosen.target_stack_entry = Some(target_id); push(&mut game,bounce);
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..2).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(2).resolve_stack(&mut game, &mut agents);
        assert_eq!(game.card(ids[0]).zone, ZoneType::Hand);
        assert!(game.stack.iter().all(|e| e.id != target_id));
        assert!(game.stack.iter().any(|e| e.id == decoy_id));
    }
    #[test]
    fn missing_target_does_not_bounce_the_source_or_a_decoy() {
        let mut game = GameState::new(&["A", "B"], 40);
        let player = PlayerId(0);
        let mut ids = Vec::new();
        for name in ["Decoy", "Reversal"] {
            let id = game.create_card(Card::new(CardId(0), name.into(), player,
                CardTypeLine::parse("Instant"), ManaCost::parse(""), ColorSet::COLORLESS, None, None, vec![], vec![]));
            game.move_card(id, ZoneType::Stack, player); ids.push(id);
        }
        let decoy = build_spell_ability(&game, ids[0], "SP$ Draw | Defined$ You | NumCards$ 1", player);
        let decoy_id = push(&mut game, decoy);
        let mut bounce = build_spell_ability(&game, ids[1], "DB$ ChangeZone | Origin$ Stack | Destination$ Hand | Defined$ Targeted", player);
        bounce.is_trigger = true; bounce.target_chosen.target_stack_entry = Some(999); push(&mut game,bounce);
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..2).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(2).resolve_stack(&mut game, &mut agents);
        assert_eq!(game.card(ids[0]).zone, ZoneType::Stack);
        assert!(game.stack.iter().any(|e| e.id == decoy_id));
    }

    #[test]
    fn copy_then_inherited_bounce_removes_original_and_keeps_copy() {
        let mut game = GameState::new(&["A", "B"], 40);
        let player = PlayerId(0);
        let target_rules = forge_carddb::parse_card_script("Name:Original\nTypes:Instant\nA:SP$ GainLife | Defined$ You | LifeAmount$ 1").unwrap();
        let reversal_rules = forge_carddb::parse_card_script("Name:Reversal\nTypes:Instant\nA:SP$ CopySpellAbility | ValidTgts$ Instant,Sorcery | SubAbility$ DBBounce\nSVar:DBBounce:DB$ ChangeZone | Origin$ Stack | Destination$ Hand | Defined$ Targeted").unwrap();
        let target = game.create_card(Card::from_rules(&target_rules, player));
        let reversal = game.create_card(Card::from_rules(&reversal_rules, player));
        game.move_card(target, ZoneType::Stack, player); game.move_card(reversal, ZoneType::Stack, player);
        let original = build_spell_ability(&game, target, "SP$ GainLife | Defined$ You | LifeAmount$ 1", player);
        let original_id = push(&mut game, original);
        let mut copy = build_spell_ability(&game, reversal, "SP$ CopySpellAbility | ValidTgts$ Instant,Sorcery | SubAbility$ DBBounce", player);
        copy.target_chosen.target_stack_entry = Some(original_id); push(&mut game, copy);
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..2).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(2).resolve_stack(&mut game, &mut agents);
        assert_eq!(game.card(target).zone, ZoneType::Hand);
        assert!(game.stack.iter().all(|e| e.id != original_id));
        assert!(game.stack.iter().any(|e| e.spell_ability.is_copy));
    }

}
