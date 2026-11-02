use super::EffectContext;
use crate::event::RunParams;
use crate::replacement::replacement_handler::{apply_replacements, ReplacementEvent};
use crate::replacement::ReplacementResult;
use crate::spellability::SpellAbility;
use crate::trigger::TriggerType;

/// Configure the spell ability during construction.
/// Mirrors Java `CopySpellAbilityEffect.buildSpellAbility` — sets the target zone
/// to Stack so the ability targets spells on the stack.
pub fn build_spell_ability(sa: &mut SpellAbility) {
    if sa.uses_targeting() {
        if let Some(ref mut tr) = sa.target_restrictions {
            tr.tgt_zone = vec![forge_foundation::ZoneType::Stack];
        }
    }
}

/// `SP$ CopySpellAbility` — copy the top spell on the stack.
///
/// Mirrors Java's `CopySpellAbilityEffect.java` (basic version).
/// Creates a clone of the topmost spell on the stack with the same targets.
/// Full retargeting support deferred.
///
/// # Card script examples
/// ```text
/// A:SP$ CopySpellAbility | Defined$ TopStack
/// A:SP$ CopySpellAbility | Defined$ TriggeredSpellAbility
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `CopySpellAbilityEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(CopySpellAbilityEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let controller = sa.activating_player;
    if sa.ir.optional && !ctx.agents[controller.index()].confirm_action(
        controller, Some("OptionalCopy"), "Copy this spell or ability?", &[], sa.source, sa.api,
    ) {
        return;
    }

    // Run CopySpell replacement effects before copying.
    let mut event = ReplacementEvent::CopySpell {
        player: controller,
        count: 1,
    };
    let result = apply_replacements(ctx.game, &mut event);
    if result == ReplacementResult::Skipped || result == ReplacementResult::Replaced {
        return;
    }

    let original = if let Some(target) = sa.target_chosen.target_stack_entry {
        ctx.game.stack.iter().find(|entry| entry.id == target)
            .map(|entry| entry.spell_ability.clone())
    } else if sa.defined() == Some("Parent") {
        ctx.game.stack.resolving_entry()
            .filter(|entry| entry.spell_ability.source == sa.source)
            .map(|entry| entry.spell_ability.clone())
    } else if let Some(defined) = sa.defined() {
        crate::ability::ability_utils::get_defined_spell_abilities(defined, sa, ctx.game)
            .into_iter()
            .next()
    } else {
        None
    };

    let original = match original {
        Some(spell) => spell,
        None => return,
    };
    if crate::card::card_factory::spell_ability_cant_be_copied(&ctx.game.cards, &original) {
        return;
    }

    // Clone the spell ability with same targets using CardFactory parity helper.
    let mut copy = crate::card::card_factory::copy_spell_ability_and_possibly_host(
        ctx.game, &original, controller,
    );
    let params = crate::parsing::Params::from_raw(&sa.ability_text);
    if params.get("MayChooseTarget").is_some_and(|v| v.eq_ignore_ascii_case("True"))
        && ctx.agents[controller.index()].confirm_action(
            controller, Some("CopyRetarget"), "Choose new targets for the copy?", &[], sa.source, sa.api,
        )
    {
        let original_targets = copy.clone();
        if !copy.setup_targets(ctx.game, ctx.agents, ctx.mana_pools) {
            copy = original_targets;
        }
    }

    // Push the copy onto the stack (it will resolve like a normal spell)
    let copy_entry = crate::spellability::StackEntry {
        id: 0, // will be assigned by push()
        spell_ability: copy,
        is_pending_cast: false,
        is_creature_spell: original.is_spell
            && original
                .source
                .is_some_and(|cid| ctx.game.card(cid).is_creature()),
        is_permanent_spell: original.is_spell
            && original
                .source
                .is_some_and(|cid| ctx.game.card(cid).is_permanent()),
        cast_from_zone: None,
        optional_trigger_decider: None,
        optional_trigger_description: None,
        optional_trigger_source_name: None,
    };

    let trigger_sa = copy_entry.spell_ability.clone();
    ctx.game.stack.push(copy_entry);
    if let Some(source_id) = trigger_sa.source {
        ctx.trigger_handler.run_trigger(
            TriggerType::SpellCopied,
            RunParams {
                spell_card: Some(source_id),
                spell_controller: Some(controller),
                source_sa: Some(trigger_sa.clone()),
                ..Default::default()
            },
            false,
        );
        super::emit_targeting_triggers(ctx, source_id, &trigger_sa);
    }
}

#[cfg(test)]
mod copy_target_regression {
    use crate::{agent::{PassAgent, PlayerAgent}, card::Card, game::GameState,
        game_loop::GameLoop, ids::{CardId, PlayerId}, spellability::build_spell_ability,
        zone::magic_stack::StackEntry};
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};

    fn entry(sa: crate::spellability::SpellAbility) -> StackEntry {
        StackEntry { id: 0, spell_ability: sa, is_creature_spell: false, is_permanent_spell: false,
            is_pending_cast: false, cast_from_zone: None, optional_trigger_decider: None,
            optional_trigger_description: None, optional_trigger_source_name: None }
    }

    #[test]
    fn copy_uses_target_stack_entry_instead_of_topmost_decoy() {
        let mut game = GameState::new(&["A", "B"], 40);
        let player = PlayerId(0);
        let mut ids = Vec::new();
        for name in ["Target", "Decoy", "Copier"] {
            let id = game.create_card(Card::new(CardId(0), name.into(), player,
                CardTypeLine::parse("Instant"), ManaCost::parse(""), ColorSet::COLORLESS,
                None, None, vec![], vec![]));
            game.move_card(id, ZoneType::Stack, player);
            ids.push(id);
        }
        let target = build_spell_ability(&game, ids[0], "SP$ GainLife | Defined$ You | LifeAmount$ 1", player);
        let target_id = game.stack.push(entry(target));
        let decoy = build_spell_ability(&game, ids[1], "SP$ Draw | Defined$ You | NumCards$ 1", player);
        game.stack.push(entry(decoy));
        let mut copier = build_spell_ability(&game, ids[2], "DB$ CopySpellAbility", player);
        copier.is_trigger = true;
        copier.target_chosen.target_stack_entry = Some(target_id);
        game.stack.push(entry(copier));
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..2).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(2).resolve_stack(&mut game, &mut agents);
        let copied = game.stack.peek().unwrap();
        assert_eq!(copied.spell_ability.api, Some(crate::ability::api_type::ApiType::GainLife));
        assert!(copied.spell_ability.is_copy);
    }
    #[test]
    fn missing_target_does_not_copy_an_unrelated_spell() {
        let mut game = GameState::new(&["A", "B"], 40);
        let player = PlayerId(0);
        let id = game.create_card(Card::new(CardId(0), "Fixture".into(), player,
            CardTypeLine::parse("Instant"), ManaCost::parse(""), ColorSet::COLORLESS,
            None, None, vec![], vec![]));
        game.move_card(id, ZoneType::Stack, player);
        let decoy = build_spell_ability(&game, id, "SP$ Draw | Defined$ You | NumCards$ 1", player);
        game.stack.push(entry(decoy));
        let mut copier = build_spell_ability(&game, id, "DB$ CopySpellAbility", player);
        copier.is_trigger = true;
        copier.target_chosen.target_stack_entry = Some(999);
        game.stack.push(entry(copier));
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..2).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(2).resolve_stack(&mut game, &mut agents);
        let top = game.stack.peek().unwrap();
        assert!(!top.spell_ability.is_copy);
        assert_eq!(top.spell_ability.api, Some(crate::ability::api_type::ApiType::Draw));
        assert_eq!(game.stack.iter().count(), 1);
    }

}
