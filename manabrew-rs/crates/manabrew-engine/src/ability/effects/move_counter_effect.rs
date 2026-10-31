use forge_foundation::ZoneType;

use super::{resolve_numeric_svar, EffectContext};
use crate::ability::ability_ir::DefinedRef;
use crate::card::CounterType;
use crate::event::RunParams;
use crate::parsing::keys;

/// `SP$ MoveCounter` — move counters from one permanent to another.
///
/// Mirrors Java's `CountersMoveEffect.java`.
/// - `CounterType$` — type of counter to move (default P1P1).
/// - `CounterNum$` — number of counters to move (default 1).
/// - `Source$` — where to take counters from (default Self/source card).
/// - `Defined$` — where to put counters (default Targeted).
///
/// # Card script examples
/// ```text
/// A:SP$ MoveCounter | CounterType$ P1P1 | CounterNum$ 1 | Source$ Self | Defined$ Targeted
/// A:SP$ MoveCounter | CounterType$ CHARGE | CounterNum$ 2
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `MoveCounterEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(MoveCounterEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let counter_type = sa.ir.counter_type.clone().unwrap_or(CounterType::P1P1);
    let count = resolve_numeric_svar(ctx.game, sa, keys::COUNTER_NUM, 1);
    let all_types = sa.ir.counter_type_text.as_deref() == Some("All");
    let all_count = sa.ir.counter_num_text.as_deref() == Some("All");
    if count <= 0 && !all_count {
        return;
    }

    // Determine source of counters
    let from_id = match sa.ir.source_text.as_deref() {
        Some(source) if matches!(DefinedRef::parse(source), DefinedRef::Targeted) => {
            sa.target_chosen.target_card
        }
        Some(source) if matches!(DefinedRef::parse(source), DefinedRef::ParentTarget) => {
            ctx.parent_target_card
        }
        _ => sa.source, // Default: Self
    };

    // Determine destination for counters
    let to_id = sa
        .target_chosen
        .target_card
        .or_else(|| match sa.defined_ref() {
            Some(DefinedRef::SelfCard) => sa.source,
            Some(DefinedRef::ParentTarget) => ctx.parent_target_card,
            _ => None,
        });

    let from = match from_id {
        Some(id)
            if matches!(
                ctx.game.card(id).zone,
                ZoneType::Battlefield | ZoneType::Exile
            ) =>
        {
            id
        }
        _ => return,
    };
    let to = match to_id {
        Some(id)
            if matches!(
                ctx.game.card(id).zone,
                ZoneType::Battlefield | ZoneType::Exile
            ) =>
        {
            id
        }
        _ => return,
    };

    if from == to {
        return;
    }

    let counters: Vec<_> = if all_types {
        ctx.game.card(from).counters.iter().filter(|(_, n)| **n > 0).map(|(kind, n)| (kind.clone(), *n)).collect()
    } else {
        vec![(counter_type.clone(), ctx.game.card(from).counter_count(&counter_type))]
    };
    for (kind, available) in counters {
        let amount = if all_count { available } else { count.min(available) };
        if amount <= 0 || ctx.game.card(to).phased_out
            || crate::staticability::static_ability_cant_put_counter::any_cant_put_counter_on_card(&ctx.game.cards, ctx.game.card(to), &kind) { continue; }
        let moved = super::effect_context::remove_counter_with_context(ctx.game, ctx.trigger_handler, from, &kind, amount, RunParams { player: Some(sa.activating_player), ..Default::default() });
        ctx.add_counter(to, &kind, moved, sa, RunParams::default());
    }
}
