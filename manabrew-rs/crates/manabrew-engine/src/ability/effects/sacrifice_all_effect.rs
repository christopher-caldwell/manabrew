use forge_foundation::ZoneType;

use super::{matches_change_type, EffectContext};
use crate::ability::ability_ir::DefinedRef;
use crate::card::valid_filter;
use crate::event::{AbilityValue, RunParams};
use crate::ids::CardId;
use crate::trigger::TriggerType;

/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `SacrificeAllEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(SacrificeAllEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let valid_cards = sa.ir.valid_cards_selector.as_ref();
    let valid_cards_filter = sa.ir.valid_cards_text.as_deref().unwrap_or("Creature");

    // When Defined$ narrows the sacrifice to specific cards (e.g. Ashling's
    // `DelayTriggerRememberedLKI` targets the token created by the parent
    // trigger), sacrifice only those cards instead of every matching
    // permanent on the battlefield.
    let defined_cards: Option<Vec<CardId>> = if sa.ir.defined_text.as_deref() == Some("AttachedBy TriggeredCardLKICopy") {
        if sa.get_triggering_object(crate::ability::AbilityKey::AttachmentCaptureKnown).is_none() {
            eprintln!("WARNING: SacrificeAll cannot determine attachment at departure; no object selected");
        }
        let target = sa.get_triggering_cards(crate::ability::AbilityKey::AttachedAtDeparture).into_iter().next();
        let timestamp = sa.get_triggering_object(crate::ability::AbilityKey::AttachedAtDepartureTimestamp)
            .and_then(|value| value.parse::<u64>().ok());
        Some(target.filter(|&id| timestamp == Some(ctx.game.card(id).zone_timestamp)).into_iter().collect())
    } else { match sa.defined_ref() {
        Some(DefinedRef::Targeted | DefinedRef::TargetedCard | DefinedRef::ThisTargetedCard) => {
            Some(sa.target_chosen.all_target_cards())
        }
        Some(
            DefinedRef::DelayTriggerRememberedLki
            | DefinedRef::DelayTriggerRemembered
            | DefinedRef::Remembered,
        ) => {
            if matches!(sa.defined_ref(), Some(DefinedRef::Remembered)) {
                sa.source.map(|source| ctx.game.card(source).remembered_cards.clone()).unwrap_or_default()
            } else {
                let mut ids = Vec::new();
                for value in &sa.trigger_remembered {
                    if let AbilityValue::Card(cid) = value {
                        ids.push(*cid);
                    }
                }
                ids
            }
            .into()
        }
        _ => None,
    }};

    // UnlessCost$ X | UnlessPayer$ You — offer the payer a chance to pay a
    // cost to prevent the sacrifice entirely. Java's deterministic AI pays
    // unless-costs when able (auto-tapping lands for mana).
    if let Some(unless_cost_str) = sa.ir.unless_cost.as_deref() {
        let source = sa.source.unwrap_or(CardId(0));
        let cost = crate::cost::parse_cost(unless_cost_str);
        let payers = super::helpers::resolve_defined_players(
            sa.ir.unless_payer_text.as_deref().unwrap_or("You"),
            sa.activating_player,
            ctx.game,
        );
        for payer in payers {
            if ctx.game.player(payer).has_lost {
                continue;
            }
            let can_pay = can_pay_unless_cost(ctx, sa, source, payer, &cost);
            let cost_kind = cost.to_simple_string();
            let cost_display = crate::cost::to_prompt_string(&cost);
            let prompt = if cost_display.is_empty() {
                "Pay this cost?".to_string()
            } else {
                format!("Pay {cost_display}?")
            };
            ctx.agents[payer.index()].snapshot_state(ctx.game, ctx.mana_pools);
            let wants_to_pay = ctx.agents[payer.index()].pay_cost_to_prevent_effect(
                payer,
                if cost_kind.is_empty() {
                    "UnlessCost"
                } else {
                    cost_kind.as_str()
                },
                &prompt,
                sa.source,
                sa.api,
                can_pay,
                &[],
                &if sa.stack_description.is_empty() {
                    sa.rebuilt_description()
                } else {
                    sa.stack_description.clone()
                },
            );
            let paid = wants_to_pay
                && can_pay
                && super::try_pay_unless_cost(ctx, sa, source, payer, &cost);
            if paid {
                return; // Cost paid — sacrifice prevented
            }
        }
    }

    let player_ids = ctx.game.player_order.clone();
    let mut to_sacrifice: Vec<CardId> = Vec::new();
    if let Some(defined_ids) = defined_cards {
        for cid in defined_ids {
            if cid.index() < ctx.game.cards.len()
                && ctx.game.card(cid).zone == ZoneType::Battlefield
                && !ctx.game.card(cid).phased_out
            {
                to_sacrifice.push(cid);
            }
        }
    } else {
        for &pid in &player_ids {
            let zone_cards = ctx.game.cards_in_zone(ZoneType::Battlefield, pid).to_vec();
            for cid in zone_cards {
                let card = ctx.game.card(cid);
                if card.phased_out {
                    continue;
                }
                let matches = match (valid_cards, sa.source) {
                    (Some(selector), Some(source_id)) => {
                        valid_filter::matches_valid_card_selector_in_game(
                            selector,
                            card,
                            ctx.game.card(source_id),
                            ctx.game,
                        )
                    }
                    _ => matches_change_type(card, valid_cards_filter, &[]),
                };
                if matches {
                    to_sacrifice.push(cid);
                }
            }
        }
    }

    let controllers = sa.ir.controller_text.as_deref().map(|defined| {
        crate::ability::ability_utils::resolve_defined_players_with_sa(
            defined, sa, sa.activating_player, ctx.game,
        )
    });
    to_sacrifice.retain(|&cid| {
        let card = ctx.game.card(cid);
        controllers.as_ref().is_none_or(|players| players.contains(&card.controller))
            && !crate::staticability::static_ability_cant_sacrifice::cant_sacrifice(
                &ctx.game.cards, card, Some(sa), false,
            )
    });

    // Track per-controller batches to fire SacrificedOnce once per controller after
    // the whole sweep (mirrors Java GameAction.sacrifice line 2133-2138).
    let mut by_controller: std::collections::BTreeMap<crate::ids::PlayerId, Vec<CardId>> =
        std::collections::BTreeMap::new();

    if to_sacrifice.is_empty() { return; }
    let before = ctx.game.boxed_snapshot();
    let mut events = Vec::new();
    for card_id in to_sacrifice {
        if ctx.game.card(card_id).zone != ZoneType::Battlefield { continue; }
        let card=before.card(card_id);
        let controller=card.controller;let owner=card.owner;
        let p1p1=card.counter_count(&crate::card::CounterType::P1P1);
        let power=card.power();let toughness=card.toughness();let counters=card.counters.clone();
        let attached=card.attached_to.map(|id|(id,before.card(id).zone_timestamp));
        ctx.game.card_mut(card_id).lki_counters=Some(counters);
        ctx.game.card_mut(card_id).set_lki_power_toughness(Some(power),Some(toughness));
        let destination=ctx.move_card_destination(card_id,ZoneType::Graveyard,owner);
        if destination == ZoneType::Battlefield {continue;}
        events.push((TriggerType::Sacrificed,RunParams{card:Some(card_id),player:Some(controller),..Default::default()}));
        events.push((TriggerType::ChangesZone,RunParams{card:Some(card_id),card_lki:Some(card_id),origin:Some(ZoneType::Battlefield),destination:Some(destination),lki_p1p1_counters:Some(p1p1),lki_power:Some(power),lki_toughness:Some(toughness),lki_attachment_captured:true,lki_attached_to:attached,..Default::default()}));
        by_controller.entry(controller).or_default().push(card_id);
    }
    ctx.trigger_handler.match_departure_batch(&before,ctx.game,events);

    for (controller, cards) in by_controller {
        ctx.trigger_handler.run_trigger(TriggerType::SacrificedOnce, RunParams {player:Some(controller),cards:Some(cards),..Default::default()}, false);
    }
}

fn can_pay_unless_cost(
    ctx: &EffectContext,
    sa: &crate::spellability::SpellAbility,
    source: CardId,
    payer: crate::ids::PlayerId,
    cost: &crate::cost::Cost,
) -> bool {
    if cost.has_mana_cost()
        && !crate::mana::can_pay_mana_cost_with_reserved_sacrifices(
            ctx.game,
            &ctx.mana_pools[payer.index()],
            payer,
            source,
            cost,
            &[],
            None,
        )
    {
        return false;
    }

    let non_mana_cost = cost.copy_with_no_mana();
    if non_mana_cost.parts.is_empty() {
        return true;
    }
    let available =
        crate::mana::calculate_available_mana(&ctx.mana_pools[payer.index()], ctx.game, payer);
    crate::cost::can_pay_with_ability(
        &non_mana_cost,
        ctx.game,
        &available,
        source,
        payer,
        Some(sa),
    )
}

#[cfg(test)]
mod controller_scope_regression {
    use crate::{ability::{AbilityKey, ability_ir::DefinedRef}, agent::{PassAgent, PlayerAgent}, card::Card,
        game::GameState, game_loop::GameLoop, ids::{CardId, PlayerId},
        spellability::build_spell_ability, zone::magic_stack::StackEntry};
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};

    #[test]
    fn sacrifice_all_obeys_triggered_controller_and_chosen_cards() {
        for active in 1..4 {
            let mut game = GameState::new(&["A", "B", "C", "D"], 40);
            let mut cards = Vec::new();
            for owner in 0..4 {
                let mut group = Vec::new();
                for _ in 0..3 {
                    let player = PlayerId(owner);
                    let id = game.create_card(Card::new(CardId(0), "Rat".into(), player,
                        CardTypeLine::parse("Creature Rat"), ManaCost::parse(""),
                        ColorSet::COLORLESS, Some(1), Some(1), vec![], vec![]));
                    game.move_card(id, ZoneType::Battlefield, player);
                    group.push(id);
                }
                cards.push(group);
            }
            let source = cards[0][0];
            game.card_mut(source).set_chosen_cards(cards[active as usize][..2].to_vec());
            let mut sa = build_spell_ability(&game, source,
                "DB$ SacrificeAll | ValidCards$ Creature.nonChosenCard | Controller$ TriggeredPlayer", PlayerId(0));
            sa.is_trigger = true;
            sa.set_triggering_object(AbilityKey::Player, active.to_string());
            sa.set_triggering_object(AbilityKey::TriggeredPlayer, PlayerId(active));
            game.stack.push(StackEntry { id: 1, spell_ability: sa, is_creature_spell: false,
                is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
                optional_trigger_decider: None, optional_trigger_description: None,
                optional_trigger_source_name: None });
            let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
            GameLoop::new(4).resolve_stack(&mut game, &mut agents);
            for owner in 0..4 {
                for (index, &id) in cards[owner].iter().enumerate() {
                    let expected = if owner == active as usize && index == 2 { ZoneType::Graveyard } else { ZoneType::Battlefield };
                    assert_eq!(game.card(id).zone, expected, "active={active} owner={owner} index={index}");
                }
            }
        }
    }

    #[test]
    fn direct_remembered_sacrifices_only_the_remembered_object_or_nothing() {
        for remember in [false, true] {
            let mut game = GameState::new(&["A", "B", "C", "D"], 40);
            let mut bears = Vec::new();
            for owner in 0..4 {
                let player = PlayerId(owner);
                let id = game.create_card(Card::new(CardId(0), "Bear".into(), player,
                    CardTypeLine::parse("Creature Bear"), ManaCost::parse("1G"),
                    ColorSet::GREEN, Some(2), Some(2), vec![], vec![]));
                game.move_card(id, ZoneType::Battlefield, player);
                bears.push(id);
            }
            let source = game.create_card(Card::new(CardId(0), "Aura".into(), PlayerId(0),
                CardTypeLine::parse("Enchantment Aura"), ManaCost::parse("1B"),
                ColorSet::BLACK, None, None, vec![], vec![]));
            game.move_card(source, ZoneType::Graveyard, PlayerId(0));
            if remember { game.card_mut(source).add_remembered_card(bears[0]); }
            let sa = build_spell_ability(&game, source,
                "DB$ SacrificeAll | Defined$ DirectRemembered", PlayerId(2));
            assert!(matches!(sa.defined_ref(), Some(DefinedRef::Remembered)));
            game.stack.push(StackEntry { id: 1, spell_ability: sa, is_creature_spell: false,
                is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
                optional_trigger_decider: None, optional_trigger_description: None,
                optional_trigger_source_name: None });
            let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
            GameLoop::new(4).resolve_stack(&mut game, &mut agents);
            for (index, &id) in bears.iter().enumerate() {
                assert_eq!(game.card(id).zone == ZoneType::Graveyard, remember && index == 0,
                    "remember={remember} index={index}");
            }
        }
    }
}
