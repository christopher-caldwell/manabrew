use forge_foundation::ZoneType;

use super::{matches_valid_cards_for_sa, EffectContext};

/// `SP$ ChooseCard` — player chooses card(s) from a filtered set in a zone.
///
/// Mirrors Java's `ChooseCardEffect.java`.
///
/// # Params
/// - `Amount` — how many cards to choose (default 1)
/// - `ChoiceZone` — zone to choose from (default Battlefield)
/// - `Choices` — ValidCards filter for eligible cards
/// - `RememberChosen` — if "True", add chosen to source's remembered_cards
///
/// Stores the chosen card(s) on the source card's `chosen_cards`.
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `ChooseCardEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(ChooseCardEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let source_id = match sa.source {
        Some(id) => id,
        None => return,
    };

    let players = match sa.ir.defined_text.as_deref() {
        Some(defined) => crate::ability::ability_utils::resolve_defined_players_with_sa(
            defined, sa, sa.activating_player, ctx.game,
        ),
        None => vec![sa.activating_player],
    };

    let amount: usize = sa
        .ir
        .amount
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let zone = sa.ir.choice_zone.unwrap_or(ZoneType::Battlefield);

    let raw_filter = sa.ir.choices.clone().unwrap_or_else(|| "Card".to_string());
    let greatest_remembered = raw_filter.contains(".greatestPowerControlledByRemembered");
    let filter = raw_filter.replace(".greatestPowerControlledByRemembered", ".RememberedPlayerCtrl");
    let filter_selector = if greatest_remembered { None } else { sa.ir.choices_selector.clone() };

    let min_amount = sa.ir.min_amount.as_deref()
        .and_then(|s| s.parse::<usize>().ok()).unwrap_or(amount);
    let remember = sa.ir.remember_chosen;

    // Collect valid cards in zone matching filter
    let mut valid = Vec::new();
    for &pid in &ctx.game.player_order.clone() {
        let zone_cards = ctx.game.cards_in_zone(zone, pid).to_vec();
        for cid in zone_cards {
            if (!sa.ir.target_controls || players.contains(&ctx.game.card(cid).controller))
                && matches_valid_cards_for_sa(
                ctx.game,
                sa,
                ctx.game.card(cid),
                filter_selector.as_ref(),
                &filter,
            ) {
                valid.push(cid);
            }
        }
    }

    if greatest_remembered {
        if let Some(highest) = valid.iter().map(|&id| ctx.game.card(id).power()).max() {
            valid.retain(|&id| ctx.game.card(id).power() == highest);
        }
    }
    let mut chosen = Vec::new();
    for controller in players {
        ctx.agents[controller.index()].snapshot_state(ctx.game, ctx.mana_pools);
        let max = amount.min(valid.len());
        let min = min_amount.min(max);
        let selection = ctx.agents[controller.index()]
            .choose_cards_for_effect(controller, &valid, min, max);
        for cid in selection.into_iter().take(max) {
            if valid.contains(&cid) && !chosen.contains(&cid) {
                chosen.push(cid);
            }
        }
    }

    // Store on source card
    ctx.game
        .card_mut(source_id)
        .set_chosen_cards(chosen.clone());

    // Optionally remember
    if remember {
        for &cid in &chosen {
            ctx.game.card_mut(source_id).add_remembered_card(cid);
        }
    }

    // ImprintChosen$ — `ChooseCardEffect.java:299-301`.
    if sa.ir.imprint_chosen {
        for &cid in &chosen {
            ctx.game.card_mut(source_id).add_imprinted_card(cid);
        }
    }
}

#[cfg(test)]
mod target_control_regression {
    use crate::{ability::AbilityKey, agent::{PassAgent, PlayerAgent}, card::Card,
        game::GameState, game_loop::GameLoop, ids::{CardId, PlayerId},
        spellability::build_spell_ability, zone::magic_stack::StackEntry};
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};

    #[test]
    fn triggered_player_chooses_only_controlled_creatures() {
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
            let mut sa = build_spell_ability(&game, source,
                "DB$ ChooseCard | Defined$ TriggeredPlayer | MinAmount$ 0 | Amount$ 2 | TargetControls$ True | Choices$ Creature", PlayerId(0));
            sa.is_trigger = true;
            sa.set_triggering_object(AbilityKey::TriggeredPlayer, PlayerId(active));
            game.stack.push(StackEntry { id: 1, spell_ability: sa, is_creature_spell: false,
                is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
                optional_trigger_decider: None, optional_trigger_description: None,
                optional_trigger_source_name: None });
            let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
            GameLoop::new(4).resolve_stack(&mut game, &mut agents);
            assert_eq!(game.card(source).chosen_cards, cards[active as usize][..2]);
        }
    }
    #[test]
    fn choose_then_sacrifice_keeps_two_for_each_opponent() {
        for controller in 0..4 {
        for count in 0..=3 {
            for active in 0..4 {
                if active == controller { continue; }
                let mut game = GameState::new(&["A", "B", "C", "D"], 40);
                let mut cards = Vec::new();
                for owner in 0..4 {
                    let mut group = Vec::new();
                    for _ in 0..if owner == active { count } else { 3 } {
                        let player = PlayerId(owner);
                        let id = game.create_card(Card::new(CardId(0), "Rat".into(), player,
                            CardTypeLine::parse("Creature Rat"), ManaCost::parse(""),
                            ColorSet::COLORLESS, Some(1), Some(1), vec![], vec![]));
                        game.move_card(id, ZoneType::Battlefield, player);
                        group.push(id);
                    }
                    cards.push(group);
                }
                let source = cards[controller as usize][0];
                game.card_mut(source).svars.insert("Sac".into(),
                    "DB$ SacrificeAll | ValidCards$ Creature.nonChosenCard | Controller$ TriggeredPlayer".into());
                let mut sa = build_spell_ability(&game, source,
                    "DB$ ChooseCard | Defined$ TriggeredPlayer | MinAmount$ 0 | Amount$ 2 | TargetControls$ True | Choices$ Creature | SubAbility$ Sac", PlayerId(controller));
                sa.is_trigger = true;
                sa.set_triggering_object(AbilityKey::TriggeredPlayer, PlayerId(active));
                game.stack.push(StackEntry { id: 1, spell_ability: sa, is_creature_spell: false,
                    is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
                    optional_trigger_decider: None, optional_trigger_description: None,
                    optional_trigger_source_name: None });
                let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
                    .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
                GameLoop::new(4).resolve_stack(&mut game, &mut agents);
                for (owner, group) in cards.iter().enumerate() {
                    let surviving = group.iter().filter(|&&id| game.card(id).zone == ZoneType::Battlefield).count();
                    assert_eq!(surviving, if owner == active as usize { count.min(2) } else { 3 },
                        "active={active} owner={owner} count={count}");
                }
            }
        }
        }
    }

    #[test]
    fn protected_opponent_permanents_are_not_sacrificed() {
        let mut game = GameState::new(&["A", "B", "C", "D"], 40);
        let player = PlayerId(1);
        let sigarda = forge_carddb::parse_card_script("Name:Protector\nManaCost:2 G W W\nTypes:Legendary Creature Angel\nPT:5/5\nS:Mode$ CantSacrifice | ValidCard$ Card.YouCtrl | ValidCause$ SpellAbility.OppCtrl | ForCost$ False").unwrap();
        let protector = game.create_card(Card::from_rules(&sigarda, player));
        game.move_card(protector, ZoneType::Battlefield, player);
        let mut group = vec![protector];
        for _ in 0..3 {
            let id = game.create_card(Card::new(CardId(0), "Rat".into(), player,
                CardTypeLine::parse("Creature Rat"), ManaCost::parse(""), ColorSet::COLORLESS,
                Some(1), Some(1), vec![], vec![]));
            game.move_card(id, ZoneType::Battlefield, player);
            group.push(id);
        }
        let source = game.create_card(Card::new(CardId(0), "Source".into(), PlayerId(0),
            CardTypeLine::parse("Creature Demon"), ManaCost::parse(""), ColorSet::COLORLESS,
            Some(1), Some(1), vec![], vec![]));
        game.move_card(source, ZoneType::Battlefield, PlayerId(0));
        game.card_mut(source).svars.insert("Sac".into(),
            "DB$ SacrificeAll | ValidCards$ Creature.nonChosenCard | Controller$ TriggeredPlayer".into());
        let mut sa = build_spell_ability(&game, source,
            "DB$ ChooseCard | Defined$ TriggeredPlayer | MinAmount$ 0 | Amount$ 2 | TargetControls$ True | Choices$ Creature | SubAbility$ Sac", PlayerId(0));
        sa.is_trigger = true;
        sa.set_triggering_object(AbilityKey::TriggeredPlayer, player);
        game.stack.push(StackEntry { id: 1, spell_ability: sa, is_creature_spell: false,
            is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
            optional_trigger_decider: None, optional_trigger_description: None,
            optional_trigger_source_name: None });
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        GameLoop::new(4).resolve_stack(&mut game, &mut agents);
        assert!(group.iter().all(|&id| game.card(id).zone == ZoneType::Battlefield));
    }

    #[test]
    fn multiple_independent_triggers_do_not_sacrifice_extra_creatures() {
        let mut game = GameState::new(&["A", "B", "C", "D"], 40);
        let mut groups = Vec::new();
        for owner in 0..4 {
            let mut group = Vec::new();
            for _ in 0..3 {
                let player = PlayerId(owner);
                let id = game.create_card(Card::new(CardId(0), "Rat".into(), player,
                    CardTypeLine::parse("Creature Rat"), ManaCost::parse(""), ColorSet::COLORLESS,
                    Some(1), Some(1), vec![], vec![]));
                game.move_card(id, ZoneType::Battlefield, player);
                group.push(id);
            }
            groups.push(group);
        }
        for controller in [0, 2] {
            let source = groups[controller][0];
            game.card_mut(source).svars.insert("Sac".into(),
                "DB$ SacrificeAll | ValidCards$ Creature.nonChosenCard | Controller$ TriggeredPlayer".into());
            let mut sa = build_spell_ability(&game, source,
                "DB$ ChooseCard | Defined$ TriggeredPlayer | MinAmount$ 0 | Amount$ 2 | TargetControls$ True | Choices$ Creature | SubAbility$ Sac", PlayerId(controller as u32));
            sa.is_trigger = true;
            sa.set_triggering_object(AbilityKey::TriggeredPlayer, PlayerId(1));
            game.stack.push(StackEntry { id: 0, spell_ability: sa, is_creature_spell: false,
                is_permanent_spell: false, is_pending_cast: false, cast_from_zone: None,
                optional_trigger_decider: None, optional_trigger_description: None,
                optional_trigger_source_name: None });
        }
        let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4).map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>).collect();
        let mut game_loop = GameLoop::new(4);
        game_loop.resolve_stack(&mut game, &mut agents);
        game_loop.resolve_stack(&mut game, &mut agents);
        for (owner, group) in groups.iter().enumerate() {
            let surviving = group.iter().filter(|&&id| game.card(id).zone == ZoneType::Battlefield).count();
            assert_eq!(surviving, if owner == 1 { 2 } else { 3 });
        }
    }

}
