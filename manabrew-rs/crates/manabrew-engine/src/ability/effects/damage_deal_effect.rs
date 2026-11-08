use forge_foundation::ZoneType;

use super::{resolve_numeric_svar, EffectContext};
use crate::ability::ability_ir::EffectIr;
use crate::card::card_damage_map::DamageTarget;
use crate::card::card_util;
use crate::parsing::amount::AmountExpr;
use crate::parsing::keys;
use crate::spellability::SpellAbility;

/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `DamageDealEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(DamageDealEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let damage = resolve_damage_amount(ctx, sa);
    let use_damage_map = ctx.game.pending_damage_map.is_some() || sa.ir.damage_map;
    if sa.ir.damage_map {
        ctx.game.ensure_pending_damage_maps();
    }

    // For triggered abilities, resolve Defined$ for target. Some effects use
    // bare `Player` to mean "each player", so collect all defined players here
    // and fan out below instead of forcing everything through a single target.
    let chosen_players = sa.target_chosen.all_target_players();
    let target_players: Vec<_> = if !chosen_players.is_empty() {
        chosen_players
    } else if let Some(defined) = sa.defined() {
        crate::ability::ability_utils::resolve_defined_players_with_sa(
            defined,
            sa,
            sa.activating_player,
            ctx.game,
        )
    } else {
        Vec::new()
    };

    // Check source card for Infect/Wither keywords
    let (source_has_infect_keyword, source_has_wither) = if let Some(src_id) = sa.source {
        let src = ctx.game.card(src_id);
        (
            src.has_infect(),
            src.has_wither()
                || crate::staticability::static_ability_wither_damage::is_wither_damage(
                    &ctx.game.cards,
                    src,
                ),
        )
    } else {
        (false, false)
    };

    // Overload: deal damage to ALL valid creatures instead of the chosen target.
    if sa.overloaded {
        let valid_tgts = sa
            .target_restrictions
            .as_ref()
            .and_then(|restrictions| restrictions.valid_tgts.first())
            .map(String::as_str)
            .unwrap_or_default();
        let valid_tgts_selector = sa
            .target_restrictions
            .as_ref()
            .map(|restrictions| &restrictions.valid_tgts_selector);
        let all_bf: Vec<crate::ids::CardId> = ctx
            .game
            .player_order
            .clone()
            .iter()
            .flat_map(|&pid| ctx.game.cards_in_zone(ZoneType::Battlefield, pid).to_vec())
            .collect();
        for cid in all_bf {
            if ctx.game.card(cid).zone != ZoneType::Battlefield {
                continue;
            }
            if !super::matches_valid_cards_for_sa(
                ctx.game,
                sa,
                ctx.game.card(cid),
                valid_tgts_selector,
                valid_tgts,
            ) {
                continue;
            }
            // Track damage source for DamagedBy trigger filters
            if let Some(src_id) = sa.source {
                if !ctx
                    .game
                    .card(cid)
                    .damage_sources_this_turn
                    .contains(&src_id)
                {
                    ctx.game.card_mut(cid).add_damage_source_this_turn(src_id);
                }
            }
            if source_has_infect_keyword || source_has_wither {
                if use_damage_map {
                    if let Some(src_id) = sa.source {
                        if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                            map.put(src_id, DamageTarget::Card(cid), damage);
                        }
                    }
                } else if !crate::staticability::static_ability_cant_put_counter::any_cant_put_counter_on_card(
                    &ctx.game.cards,
                    ctx.game.card(cid),
                    &crate::card::CounterType::M1M1,
                ) {
                    crate::ability::effects::effect_context::add_counter_with_context(
                        ctx.game,
                        Some(ctx.trigger_handler),
                        Some(ctx.agents),
                        cid,
                        &crate::card::CounterType::M1M1,
                        damage,
                        crate::event::RunParams {
                            source_player: sa.source.map(|src_id| ctx.game.card(src_id).controller),
                            cause: Some(sa.clone()),
                            ..Default::default()
                        },
                        true,
                    );
                }
            } else if use_damage_map {
                if let Some(src_id) = sa.source {
                    if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                        map.put(src_id, DamageTarget::Card(cid), damage);
                    }
                }
            } else {
                let dealt = ctx.game.deal_damage_to_card_from(cid, damage, sa.source, false);
                if let Some(source) = sa.source { apply_noncombat_lifelink(ctx, sa, source, dealt); }
            }
            if !use_damage_map {
                ctx.trigger_handler.run_trigger(
                    crate::trigger::TriggerType::DamageDone,
                    crate::event::RunParams {
                        damage_source: sa.source,
                        damage_target_card: Some(cid),
                        damage_amount: Some(damage),
                        is_combat_damage: Some(false),
                        ..Default::default()
                    },
                    false,
                );
            }
        }
        let _ = crate::ability::spell_ability_effect::replace_dying(ctx.game, sa);
        return;
    }

    for target_player in target_players {
        let source_has_infect = if let Some(src_id) = sa.source {
            let src = ctx.game.card(src_id);
            source_has_infect_keyword
                || crate::staticability::static_ability_infect_damage::is_infect_damage(
                    ctx.game,
                    &ctx.game.cards,
                    target_player,
                    src.controller,
                )
        } else {
            false
        };
        if source_has_infect {
            // Infect: deal damage to players as poison counters
            if use_damage_map {
                if let Some(src_id) = sa.source {
                    if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                        map.put(src_id, DamageTarget::Player(target_player), damage);
                    }
                }
            } else {
                ctx.add_player_counter(
                    target_player,
                    &crate::card::CounterType::Poison,
                    damage,
                    sa,
                    crate::event::RunParams {
                        source_player: sa.source.map(|source| ctx.game.card(source).controller),
                        ..Default::default()
                    },
                );
            }
        } else if use_damage_map {
            if let Some(src_id) = sa.source {
                if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                    map.put(src_id, DamageTarget::Player(target_player), damage);
                }
            }
        } else {
            let dealt =
                ctx.game
                    .deal_damage_to_player_from(target_player, damage, sa.source, false);
            ctx.game
                .record_player_damage_assignment(sa.source, Some(target_player), dealt, false);
            super::life_lose_effect::fire_life_lost_all(ctx, target_player, dealt, sa.source);
            if let Some(source) = sa.source { apply_noncombat_lifelink(ctx, sa, source, dealt); }
        }

        // Record damage dealt by source for TotalDamageDoneByThisTurn SVar
        if !use_damage_map {
            if let Some(src_id) = sa.source {
                if damage > 0 {
                    ctx.game.card_mut(src_id).total_damage_done_this_turn += damage;
                    ctx.game
                        .card_mut(src_id)
                        .damage_history
                        .record_damage(damage, false);
                }
            }
        }

        // Fire DamageDone trigger
        if !use_damage_map {
            ctx.trigger_handler.run_trigger(
                crate::trigger::TriggerType::DamageDone,
                crate::event::RunParams {
                    damage_source: sa.source,
                    damage_target_player: Some(target_player),
                    damage_amount: Some(damage),
                    is_combat_damage: Some(false),
                    ..Default::default()
                },
                false,
            );
            ctx.trigger_handler.run_trigger(
                crate::trigger::TriggerType::DamageDoneOnce,
                crate::event::RunParams {
                    damage_target_player: Some(target_player),
                    damage_amount: Some(damage),
                    is_combat_damage: Some(false),
                    ..Default::default()
                },
                false,
            );
            ctx.trigger_handler.flush_waiting_triggers(ctx.game);
        }
    }
    let mut target_cards = sa.target_chosen.target_card.into_iter().collect::<Vec<_>>();
    target_cards.extend(card_util::get_radiance(ctx.game, sa).iter().copied());
    target_cards.sort_unstable_by_key(|cid| cid.0);
    target_cards.dedup();
    for target_card in target_cards {
        if ctx.game.card(target_card).zone == ZoneType::Battlefield {
            // Protection: prevents all damage from matching sources
            if let Some(src_id) = sa.source {
                if crate::staticability::static_ability_colorless_damage_source::target_is_protected_from_source(
                    &ctx.game.cards,
                    ctx.game.card(target_card),
                    ctx.game.card(src_id),
                ) {
                    return;
                }
            }

            // Track damage source for DamagedBy trigger filters
            if let Some(src_id) = sa.source {
                if !ctx
                    .game
                    .card(target_card)
                    .damage_sources_this_turn
                    .contains(&src_id)
                {
                    ctx.game
                        .card_mut(target_card)
                        .damage_sources_this_turn
                        .push(src_id);
                }
            }
            if source_has_infect_keyword || source_has_wither {
                // Infect/Wither: damage to creatures as -1/-1 counters
                if use_damage_map {
                    if let Some(src_id) = sa.source {
                        if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                            map.put(src_id, DamageTarget::Card(target_card), damage);
                        }
                    }
                } else if !crate::staticability::static_ability_cant_put_counter::any_cant_put_counter_on_card(
                    &ctx.game.cards,
                    ctx.game.card(target_card),
                    &crate::card::CounterType::M1M1,
                ) {
                    crate::ability::effects::effect_context::add_counter_with_context(
                        ctx.game,
                        Some(ctx.trigger_handler),
                        Some(ctx.agents),
                        target_card,
                        &crate::card::CounterType::M1M1,
                        damage,
                        crate::event::RunParams {
                            source_player: sa.source.map(|src_id| ctx.game.card(src_id).controller),
                            cause: Some(sa.clone()),
                            ..Default::default()
                        },
                        true,
                    );
                }
            } else if use_damage_map {
                if let Some(src_id) = sa.source {
                    if let Some(map) = ctx.game.pending_damage_map.as_mut() {
                        map.put(src_id, DamageTarget::Card(target_card), damage);
                    }
                }
            } else {
                let dealt = ctx.game.deal_damage_to_card_from(target_card, damage, sa.source, false);
                if let Some(source) = sa.source { apply_noncombat_lifelink(ctx, sa, source, dealt); }
            }

            // Record damage dealt by source for TotalDamageDoneByThisTurn SVar
            if !use_damage_map {
                if let Some(src_id) = sa.source {
                    if damage > 0 {
                        ctx.game.card_mut(src_id).total_damage_done_this_turn += damage;
                        ctx.game
                            .card_mut(src_id)
                            .damage_history
                            .record_damage(damage, false);
                    }
                }
            }

            // Fire DamageDone trigger
            if !use_damage_map {
                ctx.trigger_handler.run_trigger(
                    crate::trigger::TriggerType::DamageDone,
                    crate::event::RunParams {
                        damage_source: sa.source,
                        damage_target_card: Some(target_card),
                        damage_amount: Some(damage),
                        is_combat_damage: Some(false),
                        ..Default::default()
                    },
                    false,
                );
                // Fire DamageDoneOnce batch trigger for non-map (non-combat)
                // damage.  Java fires this from CardDamageMap.triggerDamageOnce
                // which is called for ALL damage paths.  Without this, "when
                // dealt damage" triggers using DamageDoneOnce (e.g. Raptor
                // Hatchling Enrage) would never fire for spell damage.
                ctx.trigger_handler.run_trigger(
                    crate::trigger::TriggerType::DamageDoneOnce,
                    crate::event::RunParams {
                        damage_target_card: Some(target_card),
                        damage_amount: Some(damage),
                        is_combat_damage: Some(false),
                        ..Default::default()
                    },
                    false,
                );
                // Pre-match damage triggers while the creature is still on the
                // battlefield.  SBAs run after resolution and would move
                // lethally damaged creatures to the graveyard, causing their
                // Enrage triggers to fail the active-zone check.
                ctx.trigger_handler.flush_waiting_triggers(ctx.game);
            }

            if sa.ir.remember_damaged_creature {
                if let Some(src_id) = sa.source {
                    let src = ctx.game.card_mut(src_id);
                    src.add_remembered_card(target_card);
                }
            }
        }
    }

    let _ = crate::ability::spell_ability_effect::replace_dying(ctx.game, sa);
}

pub(super) fn apply_noncombat_lifelink(ctx: &mut EffectContext, sa: &SpellAbility, source: crate::ids::CardId, dealt: i32) {
    let card = ctx.game.card(source);
    if dealt <= 0 || !card.has_lifelink() { return; }
    let controller = card.controller;
    if crate::staticability::static_ability_cant_gain_lose_pay_life::cant_gain_life(ctx.game, controller) { return; }
    let mut event = crate::replacement::replacement_handler::ReplacementEvent::GainLife { player: controller, amount: dealt };
    let result = crate::replacement::replacement_handler::apply_replacements(ctx.game, &mut event);
    if matches!(result, crate::replacement::ReplacementResult::Skipped | crate::replacement::ReplacementResult::Replaced) { return; }
    let amount = if let crate::replacement::replacement_handler::ReplacementEvent::GainLife { amount, .. } = event { amount } else { dealt };
    if amount <= 0 { return; }
    let gained = ctx.game.player_gain_life(controller, amount);
    if gained <= 0 { return; }
    ctx.trigger_handler.run_trigger(crate::trigger::TriggerType::LifeGained, crate::event::RunParams {
        player: Some(controller), life_amount: Some(gained), source_card: Some(source), source_sa: Some(sa.clone()),
        first_time: Some(ctx.game.player(controller).life_gained_this_turn == gained), ..Default::default()
    }, false);
}

/// Resolve the NumDmg$ parameter, supporting both integer literals and SVar
/// references (e.g. `NumDmg$ X` where `SVar:X:ParentTargeted$CardPower`).
/// Mirrors Java's `AbilityUtils.calculateAmount(sa, "NumDmg", sa)`.
fn resolve_damage_amount(ctx: &EffectContext, sa: &SpellAbility) -> i32 {
    if let Some(EffectIr::DealDamage(ir)) = &sa.ir.effect {
        if let Some(amount) = &ir.amount {
            if let Some(value) = resolve_amount_expr(ctx, sa, amount) {
                #[cfg(debug_assertions)]
                debug_assert_eq!(
                    value,
                    resolve_damage_amount_from_params(ctx, sa),
                    "compiled DealDamage amount diverged from string params"
                );
                return value;
            }
        }
    }

    resolve_damage_amount_from_params(ctx, sa)
}

fn resolve_amount_expr(ctx: &EffectContext, sa: &SpellAbility, amount: &AmountExpr) -> Option<i32> {
    match amount {
        AmountExpr::Literal(value) => Some(*value),
        AmountExpr::X => Some(resolve_x_amount(ctx, sa)),
        // The local fast path does not evaluate arithmetic on nested SVars.
        // Let the shared resolver handle those expressions (for example,
        // Crackle with Power's `SVar$X/Times.5`) rather than returning zero.
        AmountExpr::SVar(name) => {
            let has_math = sa.source
                .and_then(|source| ctx.game.card(source).get_s_var(name))
                .is_some_and(|expression| expression.contains('/'));
            if has_math { None } else { Some(resolve_svar_amount(ctx, sa, name)) }
        },
        AmountExpr::Raw(_) => None,
    }
}

fn resolve_damage_amount_from_params(ctx: &EffectContext, sa: &SpellAbility) -> i32 {
    resolve_numeric_svar(ctx.game, sa, keys::NUM_DMG, 0)
}

fn resolve_x_amount(ctx: &EffectContext, sa: &SpellAbility) -> i32 {
    if let Some(source_id) = sa.source {
        if let Some(svar_expr) = ctx.game.card(source_id).get_s_var("X") {
            return evaluate_svar_expr(ctx, sa, svar_expr);
        }
    }
    sa.x_mana_cost_paid as i32
}

fn resolve_svar_amount(ctx: &EffectContext, sa: &SpellAbility, var_name: &str) -> i32 {
    if let Some(source_id) = sa.source {
        let svar_val = ctx
            .game
            .card(source_id)
            .get_s_var(var_name)
            .map(str::to_string);
        if let Some(expr) = svar_val {
            return evaluate_svar_expr(ctx, sa, &expr);
        }
    }

    0
}

/// Evaluate a simple SVar expression string.
/// Mirrors Java's `AbilityUtils.calculateAmount` for common SVar patterns.
fn evaluate_svar_expr(ctx: &EffectContext, sa: &SpellAbility, expr: &str) -> i32 {
    // Count$Kicked.X.Y — delegate to shared evaluator
    if expr.starts_with("Count$Kicked.") {
        return super::evaluate_svar(expr, sa);
    }
    // Count$ expressions — delegate to shared game-aware resolver
    if expr.starts_with("Count$") {
        if let Some(source_id) = sa.source {
            return crate::svar::resolve_count_svar_for_sa(
                expr,
                ctx.game,
                source_id,
                sa.activating_player,
                sa,
            );
        }
    }
    // Sacrificed$CardPower / Sacrificed$CardToughness — LKI from cost payment.
    // Used by Rite of Consumption: SVar:X:Sacrificed$CardPower
    if expr == "Sacrificed$CardPower" || expr == "Sacrificed$CardToughness" {
        if let Some(sac_id) = ctx.game.last_sacrificed_card {
            let sac_card = ctx.game.card(sac_id);
            let val = if expr.ends_with("Power") {
                sac_card
                    .lki_power
                    .unwrap_or(sac_card.base_power.unwrap_or(0))
            } else {
                sac_card
                    .lki_toughness
                    .unwrap_or(sac_card.base_toughness.unwrap_or(0))
            };
            return val;
        }
        return 0;
    }
    if expr == "TriggeredCard$CardPower" || expr == "TriggeredCard$CardToughness" {
        let trigger_value_key = if expr.ends_with("CardPower") {
            "TriggeredCardPower"
        } else {
            "TriggeredCardToughness"
        };
        if let Some(value) = crate::ability::ability_key::from_string(trigger_value_key)
            .and_then(|key| sa.get_triggering_value(key))
            .and_then(|value| value.trim().parse::<i32>().ok())
        {
            return value;
        }

        let triggered_card = sa
            .get_triggering_card(crate::ability::AbilityKey::Card)
            .or(sa.trigger_source);
        if let Some(card_id) = triggered_card {
            return if expr.ends_with("CardPower") {
                crate::lki::resolve_lki_power(ctx.game, card_id)
            } else {
                crate::lki::resolve_lki_toughness(ctx.game, card_id)
            };
        }
        return 0;
    }
    match expr {
        // X mana cost paid value
        "Count$xPaid" | "Count$XPaid" => sa.x_mana_cost_paid as i32,
        // Power / toughness of the parent SA's chosen target card.
        // Used by Ram Through: SVar:X:ParentTargeted$CardPower
        "ParentTargeted$CardPower" => ctx
            .parent_target_card
            .map(|id| ctx.game.card(id).power())
            .unwrap_or(0),
        "ParentTargeted$CardToughness" => ctx
            .parent_target_card
            .map(|id| ctx.game.card(id).toughness())
            .unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod numeric_damage_regressions {
    use super::*;
    use crate::det::HashMap;
    use crate::agent::{PassAgent, PlayerAgent};
    use crate::card::Card;
    use crate::game::GameState;
    use crate::ids::PlayerId;
    use crate::mana::ManaPool;
    use crate::trigger::handler::TriggerHandler;
    use forge_carddb::parse_card_script;
    use forge_foundation::ZoneType;

    #[test]
    fn named_svar_arithmetic_and_other_damage_amounts() {
        let mut game = GameState::new(&["A", "B", "C", "D"], 40);
        let player = PlayerId(0);
        let rules = parse_card_script("Name:Damage Fixture\nTypes:Sorcery\nA:SP$ DealDamage | NumDmg$ Y\nSVar:X:Count$xPaid\nSVar:Y:SVar$X/Times.5").unwrap();
        let source = game.create_card(Card::from_rules(&rules, player));
        game.move_card(source, ZoneType::Stack, player);
        let mut scaled = crate::spellability::build_spell_ability(&game, source, "SP$ DealDamage | NumDmg$ Y", player);
        scaled.x_mana_cost_paid = 4;
        let copied = scaled.clone();
        let mut x_only = crate::spellability::build_spell_ability(&game, source, "SP$ DealDamage | NumDmg$ X", player);
        x_only.x_mana_cost_paid = 4;
        let fixed = crate::spellability::build_spell_ability(&game, source, "SP$ DealDamage | NumDmg$ 3", player);
        let mut trigger_handler = TriggerHandler::new();
        let mut agents: Vec<Box<dyn PlayerAgent>> = vec![Box::new(PassAgent), Box::new(PassAgent), Box::new(PassAgent), Box::new(PassAgent)];
        let mut pools = vec![ManaPool::default(); 4];
        let templates = HashMap::default(); let variants = HashMap::default();
        let fallback = HashMap::default(); let editions = HashMap::default();
        let mut rng = crate::game_rng::ThreadRngAdapter;
        let mut ctx = EffectContext { game: &mut game, combat: None, agents: &mut agents, trigger_handler: &mut trigger_handler,
            token_templates: &templates, token_art_variants: &variants, token_fallback: &fallback,
            edition_dates: &editions, mana_pools: &mut pools, parent_target_card: None, rng: &mut rng };
        assert_eq!(resolve_damage_amount(&ctx, &scaled), 20);
        assert_eq!(resolve_damage_amount(&ctx, &copied), 20);
        assert_eq!(resolve_damage_amount(&ctx, &x_only), 4);
        assert_eq!(resolve_damage_amount(&ctx, &fixed), 3);
        for player in [PlayerId(1), PlayerId(2), PlayerId(3)] {
            scaled.target_chosen.add(None, Some(player));
        }
        let copied_targets = scaled.clone();
        <DamageDealEffect as crate::ability::spell_ability_effect::SpellAbilityEffect>::resolve(&mut ctx, &scaled);
        assert_eq!((1..4).map(|i| ctx.game.player(PlayerId(i)).life).collect::<Vec<_>>(), vec![20, 20, 20]);
        <DamageDealEffect as crate::ability::spell_ability_effect::SpellAbilityEffect>::resolve(&mut ctx, &copied_targets);
        assert_eq!((1..4).map(|i| ctx.game.player(PlayerId(i)).life).collect::<Vec<_>>(), vec![0, 0, 0]);
    }
}
