//! Skill puzzles: fixed game positions with a known right play, put to the real bot.
//!
//! Run `cargo test -p manabot skill_puzzles -- --nocapture` for the scoreboard. Each puzzle builds a
//! position the way the other tests here do, asks `SimpleAi` for its decision and compares it with the
//! right play. The test passes while every result matches `KNOWN_FAILURES`; a puzzle that starts passing
//! or newly fails stops the test so the list is updated on purpose (see docs: BOT_SKILL_PLAN.md, step 1).
//!
//! Most puzzles come as a pair: the play that is right here, and a mirror position where it is wrong, so
//! a bot that always does (or never does) the thing cannot pass both.

use super::SimpleAi;
use crate::agent::BotAgent;
use manabrew_protocol::game::{
    CardDto, CardView, GameViewDto, PlayerDto, StackObjectDto, StepKind, ZoneDto, ZoneKind,
};
use manabrew_protocol::transport::AgentPrompt;
use serde_json::{json, Value};

/// Puzzles the bot gets wrong today. Update this list when a step fixes (or breaks) one.
const KNOWN_FAILURES: &[&str] = &[
    "1a-counter-minor",
    "1c-counter-best",
    "2b-living-death-no",
    "3a-hold-answer",
    "4-attack-leader",
    "6a-removal-engine",
    "6b-removal-drain",
    "8b-windfall-no",
    "10a-protect-key",
];

struct Puzzle {
    id: &'static str,
    name: &'static str,
    expected: &'static str,
    play: fn() -> String,
}

// ---- Building positions ----

fn player(id: usize, life: i32) -> PlayerDto {
    serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":life,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()
}
fn card(
    id: &str,
    controller: usize,
    types: &[&str],
    mana_cost: &str,
    cmc: i32,
    text: &str,
) -> CardDto {
    CardDto {
        id: id.into(),
        controller_id: format!("player-{controller}"),
        owner_id: format!("player-{controller}"),
        types: types.iter().map(|t| t.to_string()).collect(),
        mana_cost: mana_cost.into(),
        cmc,
        text: text.into(),
        ..Default::default()
    }
}
fn creature(id: &str, controller: usize, pt: (i32, i32), cmc: i32, text: &str) -> CardDto {
    CardDto {
        power: Some(pt.0.to_string()),
        toughness: Some(pt.1.to_string()),
        ..card(id, controller, &["Creature"], "", cmc, text)
    }
}
fn spell(id: &str, types: &[&str], mana_cost: &str, cmc: i32, text: &str) -> CardDto {
    card(id, 0, types, mana_cost, cmc, text)
}
fn lands(first_id: usize, owner: usize, subtype: &str, count: usize) -> Vec<CardDto> {
    (0..count)
        .map(|i| CardDto {
            subtypes: vec![subtype.into()],
            ..card(
                &format!("card-{}", first_id + i),
                owner,
                &["Land"],
                "",
                0,
                "",
            )
        })
        .collect()
}
fn creatures(first_id: usize, owner: usize, pt: (i32, i32), count: usize) -> Vec<CardDto> {
    (0..count)
        .map(|i| creature(&format!("card-{}", first_id + i), owner, pt, 4, ""))
        .collect()
}
fn hidden_hand(owner: usize, count: usize) -> (ZoneKind, usize, Vec<CardView>) {
    (
        ZoneKind::Hand,
        owner,
        (0..count)
            .map(|i| CardView::Hidden {
                id: format!("hidden-{owner}-{i}"),
            })
            .collect(),
    )
}
fn zone(kind: ZoneKind, owner: usize, cards: Vec<CardDto>) -> (ZoneKind, usize, Vec<CardView>) {
    (
        kind,
        owner,
        cards.into_iter().map(CardView::Visible).collect(),
    )
}
fn view(
    active: usize,
    step: StepKind,
    lives: [i32; 4],
    zones: Vec<(ZoneKind, usize, Vec<CardView>)>,
) -> GameViewDto {
    let mut view = GameViewDto::default();
    view.active_player_id = format!("player-{active}");
    view.step = step;
    for (i, life) in lives.into_iter().enumerate() {
        view.players.push(player(i, life));
    }
    for (kind, owner, cards) in zones {
        view.zones.push(ZoneDto {
            zone: kind,
            owner_id: format!("player-{owner}"),
            count: cards.len(),
            cards,
        });
    }
    view
}
fn on_stack(controller: usize, source: CardDto, targets: Value) -> StackObjectDto {
    StackObjectDto {
        id: format!("stack-{}", source.id),
        controller_id: format!("player-{controller}"),
        is_spell: true,
        text: source.text.clone(),
        source_card: Some(source),
        targets: serde_json::from_value(targets).unwrap(),
        ..Default::default()
    }
}
fn bot_seeing(view: GameViewDto) -> SimpleAi {
    let mut bot = SimpleAi::default();
    bot.observe(view);
    bot
}

// ---- Asking the bot ----

fn decide(bot: &mut SimpleAi, input: Value, source: Option<(&str, &str)>) -> Value {
    let mut prompt = json!({"promptId":1,"decidingPlayerId":"player-0","input":input});
    if let Some((id, text)) = source {
        prompt["sourceCard"] = json!({"id":id,"text":text});
        prompt["sourceAbilityText"] = json!(text);
    }
    let prompt: AgentPrompt = serde_json::from_value(prompt).unwrap();
    serde_json::to_value(bot.decide(prompt).unwrap()).unwrap()["output"].clone()
}

/// The action prompt with one "cast" per `(card id, label)`; returns the label cast, or "hold".
fn cast_or_hold(bot: &mut SimpleAi, options: &[(&str, &str)]) -> String {
    let actions: Vec<_> = options.iter().map(|(card, _)| json!({"type":"cast","id":format!("cast:{card}:normal"),"cardId":card,"mode":{"type":"normal"},"label":"Cast normally"})).collect();
    let out = decide(bot, json!({"type":"chooseAction","actions":actions}), None);
    match out["actionId"].as_str() {
        Some(id) => options
            .iter()
            .find(|(card, _)| id == format!("cast:{card}:normal"))
            .map_or("cast something else", |(_, label)| label)
            .to_string(),
        None => "hold".into(),
    }
}

/// A target prompt for a spell the bot has just started; returns the label of the target chosen.
fn pick_target(
    bot: &mut SimpleAi,
    source: (&str, &str),
    intent: &str,
    candidates: &[(&str, &str, &str)],
) -> String {
    bot.pending_voluntary_cast = Some(source.0.into());
    let list: Vec<_> = candidates
        .iter()
        .map(|(id, kind, _)| json!({"id":id,"kind":kind}))
        .collect();
    let out = decide(
        bot,
        json!({"type":"chooseBoardTargets","presentation":{"title":"Choose a target","targets":[]},"candidates":list,
        "hostile":true,"intent":intent,"minTargets":1,"maxTargets":1,"chosenTargets":0,"cancellable":true}),
        Some(source),
    );
    match out["chosen"][0]["id"].as_str() {
        Some(id) => candidates
            .iter()
            .find(|(c, _, _)| *c == id)
            .map_or("an unlisted target", |(_, _, label)| label)
            .to_string(),
        None => "no target".into(),
    }
}

/// The attack prompt; returns "no attack", or who was attacked and by how many of the attackers.
fn attack_plan(bot: &mut SimpleAi, attackers: &[&str], targets: &[usize]) -> Vec<(String, String)> {
    let valid: Vec<_> = targets.iter().map(|t| format!("player-{t}")).collect();
    let list: Vec<_> = attackers
        .iter()
        .map(|a| json!({"attackerId":a,"mustAttack":false,"validTargetIds":valid}))
        .collect();
    let attack_targets: Vec<_> = valid
        .iter()
        .map(|t| json!({"id":t,"kind":"player","label":t}))
        .collect();
    let out = decide(
        bot,
        json!({"type":"chooseAttackers","attackers":list,"attackTargets":attack_targets}),
        None,
    );
    out["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["attackerId"].as_str().unwrap().to_string(),
                a["targetId"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn block_count(bot: &mut SimpleAi, attacker: &str, blockers: &[&str]) -> usize {
    let out = decide(
        bot,
        json!({"type":"chooseBlockers","availableBlockerIds":blockers,
        "attackers":[{"attackerId":attacker,"validBlockerIds":blockers,"minBlockers":0,"mustBeBlocked":false}]}),
        None,
    );
    out["assignments"].as_array().unwrap().len()
}

fn cast_label(cast: bool) -> String {
    if cast { "cast" } else { "hold" }.into()
}

// ---- Cards used ----

const COUNTERSPELL: &str = "Counter target spell.";
const GAIN_LIFE: &str = "You gain 3 life.";
/// An eight-mana creature that ends the game when the opponent already has a wide board.
const FINISHER: &str = "Haste\nWhen this creature enters, creatures you control get +X/+X and gain trample until end of turn, where X is the number of creatures you control.";
const LIVING_DEATH: &str = "Each player exiles all creature cards from their graveyard, then sacrifices all creatures they control, then puts all cards they exiled this way onto the battlefield.";
const WRATH: &str = "Destroy all creatures. They can't be regenerated.";
const DESTROY: &str = "Destroy target creature.";
const INSTANT_ANSWER: &str = "Destroy target creature an opponent controls.";
const ENGINE: &str = "Whenever a creature enters the battlefield under your control, draw a card and each opponent loses 1 life.";
const WINDFALL: &str = "Each player discards their hand, then draws cards equal to the greatest number of cards a player discarded this way.";
const PROTECTION: &str =
    "Target creature you control gains hexproof and indestructible until end of turn.";
const DRAIN: &str = "At the beginning of your upkeep, each opponent loses 3 life.";
const DESTROY_PERMANENT: &str = "Destroy target permanent.";

// ---- The puzzles ----

fn counter_minor_spell() -> String {
    let mut battlefield = lands(20, 0, "Island", 3);
    battlefield.extend(creatures(40, 1, (3, 3), 2));
    let mut bot = bot_seeing({
        let mut v = view(
            1,
            StepKind::Main1,
            [40; 4],
            vec![
                zone(ZoneKind::Battlefield, 0, battlefield),
                zone(
                    ZoneKind::Hand,
                    0,
                    vec![spell("card-9", &["Instant"], "{U}{U}", 2, COUNTERSPELL)],
                ),
            ],
        );
        v.stack.push(on_stack(
            1,
            spell("card-30", &["Instant"], "{W}", 1, GAIN_LIFE),
            json!([]),
        ));
        v
    });
    cast_or_hold(&mut bot, &[("card-9", "counter it")])
}

fn counter_game_winning_spell() -> String {
    let mut battlefield = lands(20, 0, "Island", 3);
    battlefield.extend(creatures(40, 1, (4, 4), 6));
    let mut bot = bot_seeing({
        let mut v = view(
            1,
            StepKind::Main1,
            [10, 40, 40, 40],
            vec![
                zone(ZoneKind::Battlefield, 0, battlefield),
                zone(
                    ZoneKind::Hand,
                    0,
                    vec![spell("card-9", &["Instant"], "{U}{U}", 2, COUNTERSPELL)],
                ),
            ],
        );
        let mut finisher = creature("card-31", 1, (6, 6), 8, FINISHER);
        finisher.mana_cost = "{6}{R}{R}".into();
        v.stack.push(on_stack(1, finisher, json!([])));
        v
    });
    cast_or_hold(&mut bot, &[("card-9", "counter it")])
}

fn counter_picks_best_threat() -> String {
    let mut finisher = creature("card-31", 1, (6, 6), 8, FINISHER);
    finisher.mana_cost = "{6}{R}{R}".into();
    let mut bot = bot_seeing({
        let mut v = view(
            1,
            StepKind::Main1,
            [10, 40, 40, 40],
            vec![zone(ZoneKind::Battlefield, 1, creatures(40, 1, (4, 4), 6))],
        );
        v.stack.push(on_stack(
            1,
            spell("card-30", &["Instant"], "{W}", 1, GAIN_LIFE),
            json!([]),
        ));
        v.stack.push(on_stack(1, finisher, json!([])));
        v
    });
    pick_target(
        &mut bot,
        ("card-9", COUNTERSPELL),
        "counter",
        &[
            ("stack-card-30", "spell", "the minor spell"),
            ("stack-card-31", "spell", "the game-winning spell"),
        ],
    )
}

fn mass_reanimation(yard_is_better: bool) -> String {
    let big = |first: usize, owner: usize| {
        vec![
            creature(&format!("card-{first}"), owner, (7, 7), 7, ""),
            creature(&format!("card-{}", first + 1), owner, (6, 6), 6, ""),
        ]
    };
    let small =
        |first: usize, owner: usize| vec![creature(&format!("card-{first}"), owner, (1, 1), 1, "")];
    let (mine_yard, mine_board, theirs_yard, theirs_board) = if yard_is_better {
        (big(50, 0), small(60, 0), small(70, 1), big(80, 1))
    } else {
        (small(50, 0), big(60, 0), big(70, 1), small(80, 1))
    };
    let mut battlefield = lands(20, 0, "Swamp", 6);
    battlefield.extend(mine_board);
    let mut bot = bot_seeing(view(
        0,
        StepKind::Main1,
        [40; 4],
        vec![
            zone(ZoneKind::Battlefield, 0, battlefield),
            zone(ZoneKind::Battlefield, 1, theirs_board),
            zone(ZoneKind::Graveyard, 0, mine_yard),
            zone(ZoneKind::Graveyard, 1, theirs_yard),
            zone(
                ZoneKind::Hand,
                0,
                vec![spell("card-9", &["Sorcery"], "{3}{B}{B}", 5, LIVING_DEATH)],
            ),
        ],
    ));
    cast_label(cast_or_hold(&mut bot, &[("card-9", "cast")]) == "cast")
}

fn hold_up_the_answer(board_is_threatening: bool) -> String {
    let mut battlefield = lands(20, 0, "Plains", 4);
    battlefield.push(creature("card-5", 0, (2, 2), 3, ""));
    let opposing = if board_is_threatening {
        creatures(40, 1, (6, 6), 3)
    } else {
        vec![]
    };
    let mut bot = bot_seeing(view(
        0,
        StepKind::Main1,
        [14, 40, 40, 40],
        vec![
            zone(ZoneKind::Battlefield, 0, battlefield),
            zone(ZoneKind::Battlefield, 1, opposing),
            zone(
                ZoneKind::Hand,
                0,
                vec![
                    spell("card-8", &["Instant"], "{2}{W}", 3, INSTANT_ANSWER),
                    card("card-9", 0, &["Creature"], "{1}{W}", 2, ""),
                ],
            ),
        ],
    ));
    // The permanent costs two of the four lands, leaving too little for the three-mana answer.
    match cast_or_hold(&mut bot, &[("card-9", "tap out for the minor permanent")]).as_str() {
        "hold" => "hold the mana".into(),
        other => other.into(),
    }
}

fn attack_closest_to_winning() -> String {
    let mut bot = bot_seeing(view(
        0,
        StepKind::CombatDeclareAttackers,
        [20, 40, 40, 40],
        vec![
            zone(
                ZoneKind::Battlefield,
                0,
                vec![creature("card-1", 0, (6, 6), 6, "")],
            ),
            zone(ZoneKind::Battlefield, 1, creatures(40, 1, (5, 5), 5)),
            zone(ZoneKind::Battlefield, 2, vec![]),
            zone(ZoneKind::Battlefield, 3, creatures(60, 3, (1, 1), 1)),
        ],
    ));
    let plan = attack_plan(&mut bot, &["card-1"], &[1, 2, 3]);
    match plan.first() {
        Some((_, target)) => format!("attack {target}"),
        None => "no attack".into(),
    }
}

fn board_wipe(ahead: bool) -> String {
    let mut mine = lands(20, 0, "Swamp", 4);
    let (mine_creatures, theirs) = if ahead {
        (
            creatures(30, 0, (4, 4), 5),
            vec![
                creatures(40, 1, (1, 1), 1),
                creatures(50, 2, (2, 2), 1),
                vec![],
            ],
        )
    } else {
        (
            vec![creature("card-30", 0, (1, 1), 1, "")],
            vec![
                creatures(40, 1, (5, 5), 3),
                creatures(50, 2, (4, 4), 3),
                creatures(60, 3, (4, 4), 2),
            ],
        )
    };
    mine.extend(mine_creatures);
    let mut zones = vec![
        zone(ZoneKind::Battlefield, 0, mine),
        zone(
            ZoneKind::Hand,
            0,
            vec![spell("card-9", &["Sorcery"], "{2}{B}{B}", 4, WRATH)],
        ),
    ];
    for (i, board) in theirs.into_iter().enumerate() {
        zones.push(zone(ZoneKind::Battlefield, i + 1, board));
    }
    let mut bot = bot_seeing(view(0, StepKind::Main1, [30, 40, 40, 40], zones));
    cast_label(cast_or_hold(&mut bot, &[("card-9", "cast")]) == "cast")
}

fn removal_target(case: &str) -> String {
    // The first legal target is always a plain body that looks bigger on paper.
    let (first, best, label, spell_text, kind) = match case {
        "engine" => (
            creature("card-41", 1, (3, 3), 3, ""),
            creature("card-42", 1, (1, 2), 3, ENGINE),
            "the engine",
            DESTROY,
            "destroy",
        ),
        _ => (
            creature("card-41", 1, (5, 5), 5, ""),
            card("card-42", 1, &["Enchantment"], "{2}{B}", 3, DRAIN),
            "the drain enchantment",
            DESTROY_PERMANENT,
            "destroy",
        ),
    };
    let mut bot = bot_seeing(view(
        0,
        StepKind::Main1,
        [9, 40, 40, 40],
        vec![zone(ZoneKind::Battlefield, 1, vec![first, best])],
    ));
    pick_target(
        &mut bot,
        ("card-9", spell_text),
        kind,
        &[
            ("card-41", "card", "the first legal target"),
            ("card-42", "card", label),
        ],
    )
}

fn chump_block(facing_lethal: bool) -> String {
    let mut attacker = creature("card-40", 1, (5, 5), 5, "");
    attacker.is_attacking = true;
    attacker.attacking_player_id = Some("player-0".into());
    attacker.attack_target_id = Some("player-0".into());
    let life = if facing_lethal { 4 } else { 40 };
    let mut bot = bot_seeing(view(
        1,
        StepKind::CombatDeclareBlockers,
        [life, 40, 40, 40],
        vec![
            zone(
                ZoneKind::Battlefield,
                0,
                vec![creature("card-1", 0, (1, 1), 1, "")],
            ),
            zone(ZoneKind::Battlefield, 1, vec![attacker]),
        ],
    ));
    if block_count(&mut bot, "card-40", &["card-1"]) > 0 {
        "chump block"
    } else {
        "no block"
    }
    .into()
}

fn windfall(hands_favour_it: bool) -> String {
    let (mine, theirs) = if hands_favour_it { (0, 7) } else { (6, 0) };
    let hand: Vec<_> = std::iter::once(spell("card-9", &["Sorcery"], "{2}{U}", 3, WINDFALL))
        .chain((0..mine).map(|i| {
            spell(
                &format!("card-{}", 90 + i),
                &["Sorcery"],
                "{5}",
                5,
                "Draw a card.",
            )
        }))
        .collect();
    let mut zones = vec![
        zone(ZoneKind::Battlefield, 0, lands(20, 0, "Island", 4)),
        zone(ZoneKind::Hand, 0, hand),
    ];
    for opponent in 1..4 {
        zones.push(hidden_hand(opponent, theirs));
    }
    let mut bot = bot_seeing(view(0, StepKind::Main1, [40; 4], zones));
    cast_label(cast_or_hold(&mut bot, &[("card-9", "cast")]) == "cast")
}

fn hold_back_a_blocker() -> String {
    let mut bot = bot_seeing(view(
        0,
        StepKind::CombatDeclareAttackers,
        [10, 40, 40, 40],
        vec![
            zone(
                ZoneKind::Battlefield,
                0,
                vec![
                    creature("card-1", 0, (4, 4), 4, ""),
                    creature("card-2", 0, (3, 3), 3, ""),
                ],
            ),
            zone(ZoneKind::Battlefield, 1, creatures(40, 1, (7, 7), 2)),
        ],
    ));
    let plan = attack_plan(&mut bot, &["card-1", "card-2"], &[1]);
    if plan.len() >= 2 {
        "attack with everything"
    } else {
        "keep a blocker home"
    }
    .into()
}

fn protect_key_permanent(key_is_targeted: bool) -> String {
    let mut battlefield = lands(20, 0, "Green", 3);
    battlefield.push(creature("card-1", 0, (4, 4), 5, ENGINE));
    battlefield.push(CardDto {
        identity: manabrew_protocol::game::CardIdentity {
            is_token: true,
            ..Default::default()
        },
        ..creature("card-2", 0, (1, 1), 0, "")
    });
    let target = if key_is_targeted { "card-1" } else { "card-2" };
    let mut bot = bot_seeing({
        let mut v = view(
            1,
            StepKind::Main1,
            [40; 4],
            vec![
                zone(ZoneKind::Battlefield, 0, battlefield),
                zone(
                    ZoneKind::Hand,
                    0,
                    vec![spell("card-9", &["Instant"], "{G}", 1, PROTECTION)],
                ),
            ],
        );
        v.stack.push(on_stack(
            1,
            spell("card-30", &["Instant"], "{1}{B}", 2, DESTROY),
            json!([{"id":target,"kind":"card"}]),
        ));
        v
    });
    cast_label(cast_or_hold(&mut bot, &[("card-9", "cast")]) == "cast")
}

fn puzzles() -> Vec<Puzzle> {
    vec![
        Puzzle {
            id: "1a-counter-minor",
            name: "Opponent casts a minor spell; hold the cheap counterspell",
            expected: "hold",
            play: counter_minor_spell,
        },
        Puzzle {
            id: "1b-counter-winner",
            name: "Opponent casts the spell that wins them the game; counter it",
            expected: "counter it",
            play: counter_game_winning_spell,
        },
        Puzzle {
            id: "1c-counter-best",
            name: "Two opposing spells on the stack; counter the game-winning one",
            expected: "the game-winning spell",
            play: counter_picks_best_threat,
        },
        Puzzle {
            id: "2a-living-death-yes",
            name: "Your graveyard beats the battlefield; cast Living Death",
            expected: "cast",
            play: || mass_reanimation(true),
        },
        Puzzle {
            id: "2b-living-death-no",
            name: "Your battlefield beats the graveyard; don't cast Living Death",
            expected: "hold",
            play: || mass_reanimation(false),
        },
        Puzzle {
            id: "3a-hold-answer",
            name: "Threatening board and an instant answer in hand; keep the mana up",
            expected: "hold the mana",
            play: || hold_up_the_answer(true),
        },
        Puzzle {
            id: "3b-develop",
            name: "Quiet board; spend the mana on the permanent",
            expected: "tap out for the minor permanent",
            play: || hold_up_the_answer(false),
        },
        Puzzle {
            id: "4-attack-leader",
            name: "Attack the opponent closest to winning, not the one with no blockers",
            expected: "attack player-1",
            play: attack_closest_to_winning,
        },
        Puzzle {
            id: "5a-wrath-ahead",
            name: "You are ahead on board; don't cast the board wipe",
            expected: "hold",
            play: || board_wipe(true),
        },
        Puzzle {
            id: "5b-wrath-behind",
            name: "You are far behind on board; cast the board wipe",
            expected: "cast",
            play: || board_wipe(false),
        },
        Puzzle {
            id: "6a-removal-engine",
            name: "Removal goes to the engine, not a bigger plain body listed first",
            expected: "the engine",
            play: || removal_target("engine"),
        },
        Puzzle {
            id: "6b-removal-drain",
            name:
                "Removal goes to the drain that kills you in three turns, not a bigger plain body",
            expected: "the drain enchantment",
            play: || removal_target("threat"),
        },
        Puzzle {
            id: "7a-no-chump",
            name: "Not facing lethal; don't chump-block",
            expected: "no block",
            play: || chump_block(false),
        },
        Puzzle {
            id: "7b-chump",
            name: "Facing lethal damage; chump-block",
            expected: "chump block",
            play: || chump_block(true),
        },
        Puzzle {
            id: "8a-windfall-yes",
            name: "Empty hand, opponents holding seven; cast Windfall",
            expected: "cast",
            play: || windfall(true),
        },
        Puzzle {
            id: "8b-windfall-no",
            name: "Full hand, opponents' hands empty; hold Windfall",
            expected: "hold",
            play: || windfall(false),
        },
        Puzzle {
            id: "9-keep-blocker",
            name: "The crack-back would be lethal; keep a blocker home",
            expected: "keep a blocker home",
            play: hold_back_a_blocker,
        },
        Puzzle {
            id: "10a-protect-key",
            name: "Removal aimed at your key creature; cast the protection spell",
            expected: "cast",
            play: || protect_key_permanent(true),
        },
        Puzzle {
            id: "10b-no-protect",
            name: "Removal aimed at a spare token; keep the protection spell",
            expected: "hold",
            play: || protect_key_permanent(false),
        },
    ]
}

/// One scoreboard for all puzzles. The test fails only when a result differs from `KNOWN_FAILURES`.
#[test]
fn skill_puzzles() {
    let results: Vec<_> = puzzles()
        .into_iter()
        .map(|p| {
            let actual = (p.play)();
            (p.id, p.name, p.expected, actual)
        })
        .collect();
    println!(
        "\n{:<22} {:<5} {:<34} {:<34} {}",
        "puzzle", "", "right play", "bot's play", "what it tests"
    );
    for (id, name, expected, actual) in &results {
        let pass = expected == actual;
        println!(
            "{:<22} {:<5} {:<34} {:<34} {}",
            id,
            if pass { "PASS" } else { "FAIL" },
            expected,
            actual,
            name
        );
    }
    let passed = results.iter().filter(|(_, _, e, a)| e == a).count();
    println!("\n{passed} of {} puzzles pass.", results.len());

    let mut problems = Vec::new();
    for (id, _, expected, actual) in &results {
        let fails = expected != actual;
        let listed = KNOWN_FAILURES.contains(id);
        if fails && !listed {
            problems.push(format!("{id} now fails (the bot played \"{actual}\", the right play is \"{expected}\"). Fix the bot, or add it to KNOWN_FAILURES if the change is intended."));
        } else if !fails && listed {
            problems.push(format!(
                "{id} now passes. Remove it from KNOWN_FAILURES to lock in the improvement."
            ));
        }
    }
    for id in KNOWN_FAILURES {
        if !results.iter().any(|(r, ..)| r == id) {
            problems.push(format!("KNOWN_FAILURES names {id}, which is not a puzzle."));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
