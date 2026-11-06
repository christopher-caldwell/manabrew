use crate::ability::ability_utils::resolve_defined_players_with_sa;
use crate::ability::effects::{evaluate_svar, EffectContext};
use crate::ability::spell_ability_effect::resolve_defined_cards_for_sa;
use crate::parsing::Params;
use crate::spellability::SpellAbility;
use crate::trigger::trigger::parse_trigger;

/// Mirrors Java's `DelayedTriggerEffect`.
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `DelayedTriggerEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(DelayedTriggerEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let Some(source_id) = sa.source else {
        return;
    };

    let mut next_id = 0;
    let Some(parsed) = parse_trigger(&sa.ability_text, &mut next_id) else {
        return;
    };
    let trigger_params = Params::from_raw(&sa.ability_text);
    let mode = parsed.kind;

    let mut execute_svar = if let Some(exec) = sa.ir.execute.as_deref() {
        if let Some(svar_text) = ctx.game.card(source_id).get_s_var(exec) {
            svar_text.to_string()
        } else {
            exec.to_string()
        }
    } else {
        return;
    };

    // CR 701.28: a delayed self-transform must retain the physical object and
    // transformation identity at registration, not acquire fresh identity when
    // the delayed trigger fires. These execution parameters are scoped runtime
    // metadata, analogous to Forge's StoredTransform SVar, not card-name rules.
    let execute_params = Params::from_raw(&execute_svar);
    if execute_params.get("Mode") == Some("Transform")
        && execute_params.get("Defined") == Some("Self")
    {
        let host = ctx.game.card(source_id);
        execute_svar.push_str(&format!(
            " | StoredTransform$ {} | StoredTransformZone$ {}",
            host.transform_timestamp,
            sa.trigger_source_zone_timestamp
                .or(sa.source_zone_timestamp)
                .unwrap_or(host.zone_timestamp),
        ));
    }

    let mut remembered_amount = 0;
    if sa.ir.remember_number {
        remembered_amount += ctx
            .game
            .card(source_id)
            .remembered_cmc
            .iter()
            .copied()
            .sum::<i32>();
    }
    if let Some(svar_name) = sa.ir.remember_svar_amount.as_deref() {
        if let Some(expr) = ctx.game.card(source_id).get_s_var(svar_name) {
            remembered_amount += evaluate_svar(expr, sa);
        }
    }

    // The player whose next turn activates the delayed trigger is not its
    // controller. Targeted needs this ability's chosen-player context (Emrakul).
    let controller = sa.activating_player;
    let delayed_for_player = sa.ir.delayed_trigger_defined_player_text.as_deref()
        .and_then(|defined| resolve_defined_players_with_sa(
            defined, sa, controller, ctx.game,
        ).into_iter().next());
    let mut remembered_lki_cards = Vec::new();
    let mut remembered_cards = Vec::new();
    let mut remembered_players = Vec::new();
    if let Some(definitions) = sa.ir.remember_objects.as_deref() {
        for definition in definitions.split(" & ").map(str::trim) {
            let cards = resolve_defined_cards_for_sa(ctx.game, sa, definition);
            let destination = if definition.ends_with("LKICopy") || definition == "RememberedLKI" {
                &mut remembered_lki_cards
            } else {
                &mut remembered_cards
            };
            destination.extend(cards);
            remembered_players.extend(resolve_defined_players_with_sa(
                definition,
                sa,
                sa.activating_player,
                ctx.game,
            ));
        }
        // CR 603.7c: a delayed ability must not reacquire a remembered card
        // after it leaves and returns as a new object. Keep registration-time
        // identity in the execution script, as for StoredTransform above;
        // it then survives trigger creation, stack ordering and source cleanup.
        let objects = remembered_cards.iter().chain(&remembered_lki_cards)
            .map(|id| format!("{}:{}", id.0, ctx.game.card(*id).zone_timestamp))
            .collect::<Vec<_>>()
            .join(",");
        execute_svar.push_str(&format!(" | StoredRememberedObjects$ {}",
            if objects.is_empty() { "none" } else { &objects }));
    }

    let delayed = crate::trigger::handler::DelayedTrigger {
        mode,
        trigger_mode: parsed.mode,
        params: trigger_params,
        execute_svar,
        controller,
        source_card: source_id,
        source_zone_timestamp: Some(
            sa.trigger_source_zone_timestamp
                .or(sa.source_zone_timestamp)
                .unwrap_or_else(|| ctx.game.card(source_id).zone_timestamp),
        ),
        created_turn: ctx.game.turn.turn_number,
        created_phase: ctx.game.turn.phase,
        target_card: None,
        remembered_amount,
        remembered_cards,
        remembered_players,
        remembered_lki_cards,
        sort_after_active: false,
        trigger_order: None,
    };
    if sa.ir.this_turn {
        ctx.trigger_handler
            .register_this_turn_delayed_trigger(delayed);
    } else if sa.ir.delayed_trigger_defined_player.is_some() {
        ctx.trigger_handler
            .register_player_defined_delayed_trigger(delayed_for_player.unwrap_or(controller), delayed);
    } else {
        ctx.trigger_handler.register_delayed_trigger(delayed);
    }
}

/// End-of-turn / next-turn registration callback.
pub fn run(ctx: &mut EffectContext, sa: &SpellAbility) {
    use crate::ability::spell_ability_effect::SpellAbilityEffect;
    DelayedTriggerEffect::resolve(ctx, sa);
}
