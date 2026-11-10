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

#[test]
fn visible_flying_blockers_prevent_losing_attacks_even_at_apparent_lethal() {
    let mut flyer = card("card-1", 0, &["Creature"], "", "");
    flyer.power = Some("3".into());
    flyer.toughness = Some("3".into());
    flyer.keywords = vec!["Flying".into()];
    let mut blocker = card("card-2", 1, &["Creature"], "", "");
    blocker.power = Some("6".into());
    blocker.toughness = Some("6".into());
    blocker.keywords = vec!["Flying".into()];
    let mut view = GameViewDto::default();
    view.players = [(0, 40), (1, 3)].into_iter().map(|(id, life)| serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":life,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()).collect();
    view.zones = vec![
        zone(ZoneKind::Battlefield, 0, vec![flyer]),
        zone(ZoneKind::Battlefield, 1, vec![blocker.clone()]),
    ];
    let mut bot = SimpleAi::default();
    bot.observe(view);
    let output = decide(
        &mut bot,
        None,
        json!({"type":"chooseAttackers","attackers":[{"attackerId":"card-1","mustAttack":false,"validTargetIds":["player-1"]}],"attackTargets":[{"id":"player-1","kind":"player","label":"opponent"}]}),
    );
    assert_eq!(
        output["assignments"].as_array().unwrap().len(),
        0,
        "{output}"
    );
    assert!(!bot.should_attack("card-1", "player-1"));
    assert!(bot.should_attack("card-1", "player-2"));
    blocker.tapped = true;
    bot.view.as_mut().unwrap().zones[1] = zone(ZoneKind::Battlefield, 1, vec![blocker]);
    assert!(bot.should_attack("card-1", "player-1"));
}

// Finding: attack-target-selection-ignores-visible-blocker-outcomes.
fn body(
    id: &str,
    owner: usize,
    power: i32,
    toughness: i32,
    keywords: &[&str],
    text: &str,
) -> CardDto {
    let mut c = card(id, owner, &["Creature"], "", text);
    c.cmc = 0;
    c.power = Some(power.to_string());
    c.toughness = Some(toughness.to_string());
    c.keywords = keywords.iter().map(|s| s.to_string()).collect();
    c
}
fn combat_table(
    own: Vec<CardDto>,
    one: Vec<CardDto>,
    two: Vec<CardDto>,
    life: i32,
    enemy_life: i32,
) -> GameViewDto {
    let mut v = GameViewDto::default();
    v.active_player_id = "player-0".into();
    v.step = StepKind::CombatDeclareAttackers;
    v.players=[(0,life),(1,enemy_life),(2,enemy_life)].into_iter().map(|(id,life)|serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":life,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()).collect();
    v.zones = vec![
        zone(ZoneKind::Battlefield, 0, own),
        zone(ZoneKind::Battlefield, 1, one),
        zone(ZoneKind::Battlefield, 2, two),
    ];
    v
}
fn attack_decision(
    view: GameViewDto,
    ids: &[&str],
    targets: &[&str],
) -> (SimpleAi, Vec<AttackAssignment>) {
    let mut bot = SimpleAi::default();
    bot.observe(view);
    let out = decide(
        &mut bot,
        None,
        json!({"type":"chooseAttackers","attackers":ids.iter().map(|id|json!({"attackerId":id,"mustAttack":false,"validTargetIds":targets})).collect::<Vec<_>>(),"attackTargets":targets.iter().map(|t|json!({"id":t,"kind":if t.starts_with("player-") {"player"}else{"planeswalker"},"label":t})).collect::<Vec<_>>()}),
    );
    (
        bot,
        serde_json::from_value(out["assignments"].clone()).unwrap(),
    )
}
#[test]
fn combat_plans_contrast_open_targets_holds_and_coordinated_lethal() {
    let small = body("a", 0, 3, 3, &["Flying"], "");
    let big = body("b", 1, 6, 6, &["Flying"], "");
    let (_, plan) = attack_decision(
        combat_table(vec![small.clone()], vec![big.clone()], vec![], 40, 3),
        &["a"],
        &["player-1", "player-2"],
    );
    assert_eq!(plan.len(), 1);
    assert_eq!(
        plan[0].target_id, "player-2",
        "open reachable lethal wins over apparent raw-power lethal"
    );
    let (_, plan) = attack_decision(
        combat_table(vec![small.clone()], vec![big.clone()], vec![], 40, 3),
        &["a"],
        &["player-1"],
    );
    assert!(plan.is_empty());
    let mut tapped = big.clone();
    tapped.tapped = true;
    let (_, plan) = attack_decision(
        combat_table(vec![small], vec![tapped], vec![], 40, 40),
        &["a"],
        &["player-1"],
    );
    assert_eq!(plan.len(), 1);
    let army: Vec<_> = (0..4)
        .map(|i| body(&format!("a{i}"), 0, 2, 2, &["Flying"], ""))
        .collect();
    let (_, plan) = attack_decision(
        combat_table(army, vec![big], vec![], 40, 5),
        &["a0", "a1", "a2", "a3"],
        &["player-1"],
    );
    assert_eq!(
        plan.len(),
        4,
        "one known casualty is worthwhile for sufficient closing damage"
    );
}
#[test]
fn combat_plans_split_sufficient_damage_and_apply_target_capacities_and_forcing() {
    let army: Vec<_> = (0..4)
        .map(|i| body(&format!("a{i}"), 0, 2, 2, &[], ""))
        .collect();
    let (mut bot, plan) = attack_decision(
        combat_table(army, vec![], vec![], 40, 4),
        &["a0", "a1", "a2", "a3"],
        &["player-1", "player-2"],
    );
    assert_eq!(plan.iter().filter(|a| a.target_id == "player-1").count(), 2);
    assert_eq!(plan.iter().filter(|a| a.target_id == "player-2").count(), 2);
    bot.recent_prompts.clear();
    let out = decide(
        &mut bot,
        None,
        json!({"type":"chooseAttackers","maxAttackers":2,"targetLimits":{"player-1":1,"player-2":1},"attackers":[{"attackerId":"a0","mustAttack":true,"goadedBy":"player-1","validTargetIds":["player-1","player-2"]},{"attackerId":"a1","mustAttack":true,"validTargetIds":["player-1"]},{"attackerId":"a2","mustAttack":false,"validTargetIds":["player-1","player-2"]}],"attackTargets":[{"id":"player-1","kind":"player","label":"one"},{"id":"player-2","kind":"player","label":"two"}]}),
    );
    let a = out["assignments"].as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert!(a
        .iter()
        .any(|a| a["attackerId"] == "a0" && a["targetId"] == "player-2"));
    assert!(a
        .iter()
        .any(|a| a["attackerId"] == "a1" && a["targetId"] == "player-1"));
}
#[test]
fn combat_plans_see_gang_blocks_profitable_trades_and_first_strike() {
    let a = body("a", 0, 5, 5, &[], "");
    let b = body("b", 1, 3, 3, &[], "");
    let c = body("c", 1, 3, 3, &[], "");
    let (_, plan) = attack_decision(
        combat_table(vec![a], vec![b, c], vec![], 40, 40),
        &["a"],
        &["player-1"],
    );
    assert!(
        plan.is_empty(),
        "two small blockers jointly kill the larger attacker for a cheaper loss"
    );
    let a = body("a", 0, 3, 3, &["First strike"], "");
    let b = body("b", 1, 3, 3, &[], "");
    let (_, plan) = attack_decision(
        combat_table(vec![a], vec![b], vec![], 40, 40),
        &["a"],
        &["player-1"],
    );
    assert_eq!(plan.len(), 1);
    let a = body("a", 0, 1, 1, &["Deathtouch"], "");
    let b = body("b", 1, 8, 8, &[], "");
    let (_, plan) = attack_decision(
        combat_table(vec![a], vec![b], vec![], 40, 40),
        &["a"],
        &["player-1"],
    );
    assert_eq!(
        plan.len(),
        1,
        "the defender may concede damage or an expensive trade"
    );
}
#[test]
fn combat_plans_preserve_flying_defense_but_allow_vigilance_and_a_lethal_race() {
    for (vigilance, enemy_life, expected) in [(false, 40, 0), (true, 40, 1), (false, 3, 1)] {
        let mut a = body("a", 0, 3, 3, &["Flying"], "");
        if vigilance {
            a.keywords.push("Vigilance".into());
        }
        let b = body("b", 1, 8, 8, &["Flying"], "");
        let (_, plan) = attack_decision(
            combat_table(vec![a], vec![b], vec![], 8, enemy_life),
            &["a"],
            &["player-2"],
        );
        assert_eq!(
            plan.len(),
            expected,
            "vigilance={vigilance}, life={enemy_life}"
        );
    }
    let a = body("a", 0, 3, 3, &[], "");
    let home = body("home", 0, 1, 1, &["Flying"], "");
    let b = body("b", 1, 8, 8, &["Flying"], "");
    let (_, plan) = attack_decision(
        combat_table(vec![a, home], vec![b], vec![], 8, 40),
        &["a"],
        &["player-2"],
    );
    assert_eq!(
        plan.len(),
        1,
        "a nonattacking flyer already covers flying defense"
    );
}
#[test]
fn combat_plans_value_real_trigger_mana_and_lifelink_opportunities_with_empty_hand_control() {
    for (hand, expected) in [(false, 0), (true, 1)] {
        let a = body("a", 0, 1, 1, &["Firebending:3"], "");
        let b = body("b", 1, 3, 3, &[], "");
        let mut v = combat_table(vec![a], vec![b], vec![], 40, 40);
        if hand {
            let mut spell = card("spell", 0, &["Instant"], "{R}", "Draw a card.");
            spell.cmc = 1;
            v.zones.push(zone(ZoneKind::Hand, 0, vec![spell]));
        }
        let (_, plan) = attack_decision(v, &["a"], &["player-1"]);
        assert_eq!(plan.len(), expected, "hand={hand}");
    }
    let a = body("a", 0, 3, 3, &["Lifelink"], "");
    let b = body("b", 1, 0, 6, &[], "");
    let (_, plan) = attack_decision(
        combat_table(vec![a], vec![b], vec![], 2, 40),
        &["a"],
        &["player-1"],
    );
    assert_eq!(plan.len(), 1);
    let a = body("a", 0, 1, 1, &[], "");
    let watcher = card(
        "watcher",
        0,
        &["Enchantment"],
        "",
        "Whenever a creature you control attacks, draw a card.",
    );
    let b = body("b", 1, 2, 2, &[], "");
    let mut v = combat_table(vec![a, watcher], vec![b], vec![], 40, 40);
    let mut library = zone(ZoneKind::Library, 0, vec![]);
    library.count = 20;
    v.zones.push(library);
    let (_, plan) = attack_decision(v, &["a"], &["player-1"]);
    assert_eq!(plan.len(), 1, "a cheap sacrifice can buy an actual draw");
}
#[test]
fn combat_plans_pressure_walkers_and_respect_visible_player_prevention() {
    let a = body("a", 0, 3, 3, &[], "");
    let mut walker = card("walker", 1, &["Planeswalker"], "", "");
    walker.counters.insert("Loyalty".into(), 3);
    let (_, plan) = attack_decision(
        combat_table(vec![a.clone()], vec![walker], vec![], 40, 40),
        &["a"],
        &["player-1", "walker"],
    );
    assert_eq!(plan[0].target_id, "walker");
    let mut v = combat_table(vec![a], vec![], vec![], 40, 3);
    v.players[1]
        .player_keywords
        .push("Protection from everything".into());
    let (_, plan) = attack_decision(v, &["a"], &["player-1", "player-2"]);
    assert_eq!(plan[0].target_id, "player-2");
}

#[test]
fn archived_combat_positions_avoid_no_gain_wipeouts_and_reassess_protected_opponent() {
    let cases: Value = serde_json::from_str(include_str!("attack_review_positions.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let mut bot = SimpleAi::default();
        bot.observe(serde_json::from_value(case["view"].clone()).unwrap());
        let prompt = serde_json::from_value(case["prompt"].clone()).unwrap();
        let response = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
        let plan: Vec<AttackAssignment> =
            serde_json::from_value(response["output"]["assignments"].clone()).unwrap();
        let original: Vec<AttackAssignment> =
            serde_json::from_value(case["original"]["assignments"].clone()).unwrap();
        let me = case["prompt"]["decidingPlayerId"].as_str().unwrap();
        let empty = HashSet::new();
        let before = bot.attack_plan_score(&original, me, &empty);
        let after = bot.attack_plan_score(&plan, me, &empty);
        assert!(
            after > before,
            "{}: {before} -> {after}: {response}",
            case["origin"]
        );
        if me == "player-0" {
            assert!(plan.iter().any(|a| a.target_id == "player-3"));
            assert!(!plan.iter().any(|a| a.target_id == "player-1"));
        } else {
            assert!(
                plan.iter()
                    .filter(|a| a.target_id == "player-0")
                    .all(|a| bot
                        .card(&a.attacker_id)
                        .is_some_and(|c| SimpleAi::has_keyword(c, "Myriad"))),
                "protection allows a useful myriad attack, but no direct damage: {response}"
            );
            let mut open: GameViewDto = serde_json::from_value(case["view"].clone()).unwrap();
            open.players[0].player_keywords.clear();
            bot = SimpleAi::default();
            bot.observe(open);
            let response = serde_json::to_value(
                bot.decide(serde_json::from_value(case["prompt"].clone()).unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert!(
                response["output"]["assignments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a["targetId"] == "player-0"),
                "without protection, exposed player becomes a useful target"
            );
        }
    }
}

/// Public combat projection only: final visible sizes/keywords, actual engine
/// blocker legality, damage and SBAs. Hidden responses and scripted attack/ETB
/// payoffs are deliberately outside this consequence fixture.
fn executed_combat(bot: &SimpleAi, plan: &[AttackAssignment]) -> (i32, usize, usize) {
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
    use manabrew_engine::{
        agent::{PassAgent, PlayerAgent},
        card::Card,
        combat::{CombatState, DefenderId},
        game::GameState,
        ids::{CardId, PlayerId},
    };
    let view = bot.view.as_ref().unwrap();
    let player = |id: &str| PlayerId(id.strip_prefix("player-").unwrap().parse().unwrap());
    let mut g = GameState::new(&["A", "B", "C", "D"], 40);
    let mut ids = HashMap::new();
    let mut originals = HashMap::new();
    for p in &view.players {
        let ps = g.player_mut(player(&p.id));
        ps.life = p.life;
        ps.changed_keywords = p.player_keywords.clone();
        ps.damage_prevention = p.damage_prevention;
    }
    for z in &view.zones {
        if z.zone != ZoneKind::Battlefield {
            continue;
        }
        for c in &z.cards {
            let CardView::Visible(c) = c else {
                continue;
            };
            let card = Card::new(
                CardId(0),
                c.identity.name.clone(),
                player(&c.controller_id),
                CardTypeLine::parse(&c.types.join(" ")),
                ManaCost::parse(""),
                ColorSet::COLORLESS,
                Some(SimpleAi::stat(c.power.as_deref())),
                Some(SimpleAi::stat(c.toughness.as_deref())),
                c.keywords.clone(),
                vec![],
            );
            let id = g.create_card(card);
            g.move_card(id, ZoneType::Battlefield, player(&c.controller_id));
            g.card_mut(id).tapped = c.tapped;
            g.card_mut(id).phased_out = c.phased_out;
            g.card_mut(id).damage = c.damage;
            if let Some(loyalty) = c.counters.get("Loyalty") {
                g.card_mut(id).add_counter(
                    &manabrew_engine::card::CounterType::Loyalty,
                    *loyalty as i32,
                );
            }
            ids.insert(c.id.clone(), id);
            originals.insert(c.id.clone(), c);
        }
    }
    let mut combat = CombatState::default();
    let mut defenders = HashSet::new();
    for a in plan {
        let target = if a.target_id.starts_with("player-") {
            DefenderId::Player(player(&a.target_id))
        } else {
            DefenderId::Permanent(ids[&a.target_id])
        };
        combat.add_attacker(ids[&a.attacker_id], target);
        defenders.insert(target.controlling_player(&g));
    }
    for defender in defenders {
        let owner = format!("player-{}", defender.0);
        let blockers: Vec<_> = originals
            .values()
            .filter(|c| c.controller_id == owner)
            .copied()
            .collect();
        let menu: Vec<_> = plan
            .iter()
            .filter(|a| {
                bot.card(&a.target_id)
                    .map_or(a.target_id.as_str(), |c| c.controller_id.as_str())
                    == owner
            })
            .map(|a| {
                let attacker = originals[&a.attacker_id];
                manabrew_protocol::prompts::choose_blockers::BlockableAttackerDto {
                    attacker_id: a.attacker_id.clone(),
                    valid_blocker_ids: blockers
                        .iter()
                        .filter(|b| bot.visible_can_block(b, attacker))
                        .map(|c| c.id.clone())
                        .collect(),
                    min_blockers: if SimpleAi::has_keyword(attacker, "Menace") {
                        2
                    } else {
                        1
                    },
                    max_blockers: None,
                    must_be_blocked: false,
                }
            })
            .collect();
        let blockers = bot.declare_blockers_with_targets(
            &menu,
            &blockers.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            &owner,
            Some(plan),
        );
        for block in blockers {
            assert!(manabrew_engine::combat::combat_util::can_creature_block(
                &g,
                ids[&block.blocker_id],
                ids[&block.attacker_id]
            ));
            combat.add_blocker(ids[&block.attacker_id], ids[&block.blocker_id]);
        }
    }
    let before: Vec<_> = g.players.iter().map(|p| p.life).collect();
    let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
        .map(|_| Box::new(PassAgent) as Box<dyn PlayerAgent>)
        .collect();
    for first in [true, false] {
        combat.resolve_damage_step(&mut g, &mut agents, first, &Default::default());
        g.check_state_based_actions();
        let absent: Vec<_> = g
            .cards
            .iter()
            .filter(|c| c.zone != ZoneType::Battlefield)
            .map(|c| c.id)
            .collect();
        for id in absent {
            combat.remove_from_combat(id, &mut g);
        }
    }
    let damage = before
        .iter()
        .zip(&g.players)
        .map(|(before, p)| (before - p.life).max(0))
        .sum();
    let own_dead = plan
        .iter()
        .filter(|a| g.card(ids[&a.attacker_id]).zone != ZoneType::Battlefield)
        .count();
    let opposing_dead = originals
        .iter()
        .filter(|(id, c)| {
            c.types.iter().any(|t| t == "Creature")
                && !plan.iter().any(|a| a.attacker_id == ***id)
                && g.card(ids[*id]).zone != ZoneType::Battlefield
        })
        .count();
    (damage, own_dead, opposing_dead)
}

#[test]
fn archived_army_decisions_have_better_executed_casualty_and_damage_consequences() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let cases: Value =
                serde_json::from_str(include_str!("attack_review_positions.json")).unwrap();
            for case in cases.as_array().unwrap() {
                let mut bot = SimpleAi::default();
                bot.observe(serde_json::from_value(case["view"].clone()).unwrap());
                let response = serde_json::to_value(
                    bot.decide(serde_json::from_value(case["prompt"].clone()).unwrap())
                        .unwrap(),
                )
                .unwrap();
                let plan: Vec<AttackAssignment> =
                    serde_json::from_value(response["output"]["assignments"].clone()).unwrap();
                let original: Vec<AttackAssignment> =
                    serde_json::from_value(case["original"]["assignments"].clone()).unwrap();
                let before = executed_combat(&bot, &original);
                let after = executed_combat(&bot, &plan);
                if case["view_seq"] == 19769 {
                    assert_eq!(before, (0, 5, 0));
                    assert!(after.0 > 0);
                    assert_eq!(after.1, 0);
                } else {
                    assert_eq!(before, (6, 4, 0));
                    assert!(after.1 < before.1, "{before:?} -> {after:?}");
                }
                eprintln!(
                    "{} public combat only: original {before:?}, repaired {after:?}",
                    case["origin"]
                );
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn myriad_reaches_public_boolean_prompts_and_simple_pilot_can_accept_or_decline() {
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
    use manabrew_agent_interface::{
        agent_impl::{PromptAgent, Responder},
        game_view_dto::GameViewDtoExt,
    };
    use manabrew_engine::{
        agent::{PassAgent, PlayerAgent},
        card::Card,
        game::GameState,
        game_loop::GameLoop,
        ids::{CardId, PlayerId},
        mana::ManaPool,
    };
    use std::{cell::RefCell, rc::Rc};
    struct PublicPilot {
        bot: SimpleAi,
        decline: bool,
        offers: Rc<RefCell<Vec<bool>>>,
    }
    impl Responder for PublicPilot {
        fn observe_engine(&mut self, g: &GameState, pools: &[ManaPool]) {
            self.bot.observe(GameViewDto::from_engine(
                g,
                pools,
                PlayerId(0),
                "myriad-interface-regression",
            ));
        }
        fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
            let id = prompt.prompt_id;
            let myriad = matches!(&prompt.input,PromptInput::ChooseBoolean(input) if input.presentation.title.starts_with("Create a token that's a copy"));
            let mut action = self.bot.decide(prompt).unwrap();
            if myriad {
                if self.decline {
                    action =
                        PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value: false });
                }
                let PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value }) = &action
                else {
                    panic!("missing boolean decision")
                };
                self.offers.borrow_mut().push(*value);
            }
            ClientToServerMessage::Response {
                prompt_id: id,
                action,
            }
        }
    }
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            for (decline, instances) in [(false, 1), (true, 1), (false, 2), (true, 2)] {
                let mut g = GameState::new(&["A", "B", "C", "D"], 40);
                let mut keywords = vec!["Flying".into(), "Vigilance".into()];
                keywords.extend((0..instances).map(|_| "Myriad".to_string()));
                let flyer = Card::new(
                    CardId(0),
                    "Keyword Flyer".into(),
                    PlayerId(0),
                    CardTypeLine::parse("Creature Angel"),
                    ManaCost::parse("3 W W"),
                    ColorSet::WHITE,
                    Some(4),
                    Some(4),
                    keywords,
                    vec![],
                );
                let id = g.create_card(flyer);
                g.move_card(id, ZoneType::Battlefield, PlayerId(0));
                g.card_mut(id).summoning_sick = false;
                let offers = Rc::new(RefCell::new(vec![]));
                let pilot = PromptAgent::new(
                    PlayerId(0),
                    "myriad-interface-regression".into(),
                    PublicPilot {
                        bot: SimpleAi::default(),
                        decline,
                        offers: offers.clone(),
                    },
                );
                let mut agents: Vec<Box<dyn PlayerAgent>> = vec![
                    Box::new(pilot),
                    Box::new(PassAgent),
                    Box::new(PassAgent),
                    Box::new(PassAgent),
                ];
                GameLoop::new(4).step_combat(&mut g, &mut agents);
                assert_eq!(*offers.borrow(), vec![!decline; instances * 2]);
                assert_eq!(
                    g.cards.iter().filter(|c| c.is_token).count(),
                    if decline { 0 } else { instances * 2 }
                );
                let dealt: i32 = g.players.iter().skip(1).map(|p| 40 - p.life).sum();
                assert_eq!(dealt, if decline { 4 } else { 4 + instances as i32 * 8 });
                assert!(!g
                    .cards
                    .iter()
                    .any(|c| c.is_token && c.zone == ZoneType::Battlefield));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn safe_reachable_pressure_prefers_the_dangerous_opponent_regardless_of_menu_order() {
    let a = body("a", 0, 6, 6, &["Flying", "Vigilance"], "");
    let board: Vec<_> = (0..5)
        .map(|i| body(&format!("b{i}"), 1, 5, 5, &[], ""))
        .collect();
    for targets in [vec!["player-2", "player-1"], vec!["player-1", "player-2"]] {
        let (_, plan) = attack_decision(
            combat_table(vec![a.clone()], board.clone(), vec![], 20, 40),
            &["a"],
            &targets,
        );
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].target_id, "player-1");
    }
}

#[test]
fn optional_copy_loop_guard_distinguishes_changed_stack_and_board_from_unchanged_reprompt() {
    let source = body("card-1", 0, 4, 4, &["Myriad"], "");
    let mut bot = SimpleAi::default();
    bot.observe(combat_table(vec![source.clone()], vec![], vec![], 40, 40));
    let input = json!({"type":"chooseBoolean","presentation":{"title":"Create a token that's a copy attacking the other opponent?","targets":[]},"confirmLabel":"Accept","denyLabel":"Decline"});
    assert_eq!(
        decide(&mut bot, Some(source.clone()), input.clone())["value"],
        true
    );
    assert_eq!(
        decide(&mut bot, Some(source.clone()), input.clone())["value"],
        false,
        "stable repeated choices retain loop protection"
    );
    bot.view.as_mut().unwrap().stack.push(StackObjectDto {
        id: "stack-8".into(),
        ..Default::default()
    });
    assert_eq!(
        decide(&mut bot, Some(source.clone()), input.clone())["value"],
        true,
        "a distinct pending instance is a new choice"
    );
    assert_eq!(
        decide(&mut bot, Some(source.clone()), input.clone())["value"],
        false
    );
    bot.view.as_mut().unwrap().zones[0]
        .cards
        .push(CardView::Visible(body("card-2", 0, 4, 4, &[], "")));
    assert_eq!(
        decide(&mut bot, Some(source), input)["value"],
        true,
        "new objects distinguish a completed batch"
    );
}
