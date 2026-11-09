use super::*;
use crate::agent::BotAgent;
use manabrew_protocol::game::{StackObjectDto, ZoneDto};
use serde_json::{json, Value};

fn card(id: &str, owner: usize, types: &[&str], cost: &str, text: &str) -> CardDto {
    CardDto {
        id: id.into(),
        controller_id: format!("player-{owner}"),
        owner_id: format!("player-{owner}"),
        types: types.iter().map(|s| s.to_string()).collect(),
        mana_cost: cost.into(),
        cmc: 3,
        text: text.into(),
        ..Default::default()
    }
}

fn zone(kind: ZoneKind, owner: usize, cards: Vec<CardDto>) -> ZoneDto {
    ZoneDto {
        zone: kind,
        owner_id: format!("player-{owner}"),
        count: cards.len(),
        cards: cards.into_iter().map(CardView::Visible).collect(),
    }
}

fn decide(bot: &mut SimpleAi, source: Option<CardDto>, input: Value) -> Value {
    let prompt = serde_json::from_value(
        json!({"promptId":1,"decidingPlayerId":"player-0","sourceCard":source,"input":input}),
    )
    .unwrap();
    serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
}

fn counter_table(color: &str) -> (SimpleAi, CardDto) {
    let mut spell = card("card-30", 1, &["Sorcery"], "{1}{B}", "Draw two cards.");
    spell.color = color.into();
    let mut counter = card(
        "card-9",
        0,
        &["Instant"],
        "{R}",
        "Counter target spell if it's blue.",
    );
    counter.cmc = 1;
    let mut land = card("card-1", 0, &["Land"], "", "");
    land.subtypes = vec!["Mountain".into()];
    let mut view = GameViewDto::default();
    view.active_player_id = "player-1".into();
    view.step = StepKind::Main1;
    view.zones = vec![
        zone(ZoneKind::Battlefield, 0, vec![land]),
        zone(ZoneKind::Hand, 0, vec![counter.clone()]),
    ];
    view.stack.push(StackObjectDto {
        id: "stack-1".into(),
        controller_id: "player-1".into(),
        is_spell: true,
        source_card: Some(spell),
        ..Default::default()
    });
    let mut bot = SimpleAi::default();
    bot.observe(view);
    (bot, counter)
}

#[test]
fn a_conditional_counter_is_cast_only_when_its_condition_holds() {
    for (color, expected) in [("B", "pass"), ("U", "act"), ("UR", "act"), ("C", "pass")] {
        let (mut bot, _) = counter_table(color);
        let out = decide(
            &mut bot,
            None,
            json!({"type":"chooseAction","actions":[{"type":"cast","id":"cast:card-9:normal","cardId":"card-9","mode":{"type":"normal"},"label":"Cast normally"}]}),
        );
        assert_eq!(out["type"], expected, "color {color}: {out}");
    }
}

#[test]
fn a_conditional_counter_targets_a_spell_it_will_actually_counter() {
    let (mut bot, counter) = counter_table("U");
    let mut black = bot.view.as_ref().unwrap().stack[0].clone();
    black.id = "stack-2".into();
    let source = black.source_card.as_mut().unwrap();
    source.color = "B".into();
    source.cmc = 12;
    bot.view.as_mut().unwrap().stack.push(black);
    bot.pending_voluntary_cast = Some(counter.id.clone());
    let out = decide(
        &mut bot,
        Some(counter),
        json!({"type":"chooseBoardTargets","hostile":true,"intent":"counter","cancellable":true,"minTargets":1,"maxTargets":1,"chosenTargets":0,"presentation":{"title":"Counter","targets":[]},"candidates":[{"id":"stack-2","kind":"spell"},{"id":"stack-1","kind":"spell"}]}),
    );
    assert_eq!(out["chosen"][0]["id"], "stack-1", "{out}");
}

#[test]
fn counter_colour_restrictions_use_the_public_card_for_each_colour() {
    for (letter, name) in [
        ('W', "white"),
        ('U', "blue"),
        ('B', "black"),
        ('R', "red"),
        ('G', "green"),
    ] {
        let mut source = CardDto::default();
        source.types = vec!["Instant".into()];
        source.color = letter.to_string();
        assert!(SimpleAi::counter_affects(
            &format!("counter target {name} spell."),
            Some(&source),
            true
        ));
        assert!(!SimpleAi::counter_affects(
            &format!("counter target non{name} spell."),
            Some(&source),
            true
        ));
        source.color = "C".into();
        assert!(!SimpleAi::counter_affects(
            &format!("counter target spell if it's {name}."),
            Some(&source),
            true
        ));
        assert!(SimpleAi::counter_affects(
            "counter target spell.",
            Some(&source),
            true
        ));
    }
}

#[test]
fn evasion_targets_an_own_attacker_and_cancels_when_only_opponents_are_offered() {
    for (own_offered, expected) in [(true, "boardTargets"), (false, "cancel")] {
        let source = card(
            "card-1",
            0,
            &["Land"],
            "",
            "{T}: Add {C}.\n{4}, {T}: Target creature can't be blocked this turn.",
        );
        let mut attacker = card("card-2", 0, &["Creature"], "", "");
        attacker.power = Some("3".into());
        attacker.is_attacking = true;
        attacker.attack_target_id = Some("player-1".into());
        let mut enemy = card("card-3", 1, &["Creature"], "", "");
        enemy.power = Some("20".into());
        enemy.toughness = Some("20".into());
        let mut view = GameViewDto::default();
        view.active_player_id = "player-0".into();
        view.step = StepKind::CombatDeclareAttackers;
        view.zones = vec![
            zone(ZoneKind::Battlefield, 0, vec![source.clone(), attacker]),
            zone(ZoneKind::Battlefield, 1, vec![enemy]),
        ];
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let mut candidates = vec![json!({"id":"card-3","kind":"card"})];
        if own_offered {
            candidates.push(json!({"id":"card-2","kind":"card"}));
        }
        let prompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","sourceCard":source,
            "sourceAbilityText":"Target creature can't be blocked this turn.","input":{"type":"chooseBoardTargets","hostile":false,"intent":"buff","cancellable":true,"minTargets":1,"maxTargets":1,"chosenTargets":0,"presentation":{"title":"Buff","targets":[]},"candidates":candidates}})).unwrap();
        let out = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone();
        assert_eq!(out["type"], expected, "{out}");
        if own_offered {
            assert_eq!(out["chosen"][0]["id"], "card-2", "{out}");
        }
    }
}

#[test]
fn evasion_is_activated_only_for_a_targetable_attacker_before_blocks() {
    for (active, step, attacking, shroud, blocker, expected) in [
        (0, StepKind::Upkeep, false, false, true, "pass"),
        (0, StepKind::Main1, false, false, true, "pass"),
        (
            1,
            StepKind::CombatDeclareAttackers,
            true,
            false,
            true,
            "pass",
        ),
        (
            0,
            StepKind::CombatDeclareAttackers,
            false,
            false,
            true,
            "pass",
        ),
        (
            0,
            StepKind::CombatDeclareAttackers,
            true,
            true,
            true,
            "pass",
        ),
        (
            0,
            StepKind::CombatDeclareAttackers,
            true,
            false,
            false,
            "pass",
        ),
        (
            0,
            StepKind::CombatDeclareBlockers,
            true,
            false,
            true,
            "pass",
        ),
        (
            0,
            StepKind::CombatDeclareAttackers,
            true,
            false,
            true,
            "act",
        ),
    ] {
        let mut source = card(
            "card-1",
            0,
            &["Land"],
            "",
            "{4}, {T}: Target creature can't be blocked this turn.",
        );
        source.cmc = 0;
        let mut creature = card(
            "card-2",
            0,
            &["Creature"],
            "",
            "Whatever this creature's name is.",
        );
        creature.power = Some("7".into());
        creature.toughness = Some("7".into());
        creature.is_attacking = attacking;
        creature.attack_target_id = Some("player-1".into());
        if shroud {
            creature.keywords.push("Shroud".into());
        }
        let mut enemy = card("card-3", 1, &["Creature"], "", "");
        enemy.power = Some("3".into());
        enemy.toughness = Some("3".into());
        let mut view = GameViewDto::default();
        view.active_player_id = format!("player-{active}");
        view.step = step;
        view.zones = vec![
            zone(ZoneKind::Battlefield, 0, vec![source, creature]),
            zone(
                ZoneKind::Battlefield,
                1,
                if blocker { vec![enemy] } else { vec![] },
            ),
        ];
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let out = decide(
            &mut bot,
            None,
            json!({"type":"chooseAction","actions":[{"type":"activateAbility","id":"ability:card-1:0","cardId":"card-1","abilityIndex":0,"description":"Target creature can't be blocked this turn.","cost":"{4}, {T}","isManaAbility":false}]}),
        );
        assert_eq!(
            out["type"], expected,
            "{active} {step:?} attacking {attacking} shroud {shroud} blocker {blocker}: {out}"
        );
    }
}

#[test]
fn value_protection_gets_an_end_step_window_without_a_key_permanent_or_reserve() {
    for (text, lands, expected) in [
        (
            "Create a 1/1 creature token. Permanents you control gain hexproof until end of turn.",
            2,
            "endOfTurn",
        ),
        (
            "Permanents you control gain hexproof until end of turn.",
            2,
            "main1",
        ),
        (
            "Create a 1/1 creature token. Permanents you control gain hexproof until end of turn.",
            1,
            "main1",
        ),
    ] {
        let mut protection = card("card-9", 0, &["Instant"], "{1}{U}", text);
        protection.cmc = 2;
        let mut view = GameViewDto::default();
        view.active_player_id = "player-3".into();
        view.step = StepKind::Main1;
        view.players=(0..4).map(|id|serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":40,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()).collect();
        view.zones = vec![
            zone(ZoneKind::Hand, 0, vec![protection]),
            zone(
                ZoneKind::Battlefield,
                0,
                (0..lands)
                    .map(|i| {
                        let mut land = card(&format!("card-{i}"), 0, &["Land"], "", "");
                        land.subtypes = vec!["Island".into()];
                        land
                    })
                    .collect(),
            ),
        ];
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let out = decide(&mut bot, None, json!({"type":"chooseAction","actions":[]}));
        assert_eq!(
            out["until"]["phase"], expected,
            "{text} lands {lands}: {out}"
        );
        assert!(
            bot.reserve.is_none(),
            "the board has no permanent worth reserving protection for"
        );
    }
}
