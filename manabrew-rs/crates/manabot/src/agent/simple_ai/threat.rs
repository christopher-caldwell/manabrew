//! Threat model: how close each opponent is to winning, and how much a permanent or a spell on the
//! stack matters. Pure functions over the game view; the bot's decisions (targets, attacks, counters,
//! protection) all read these two scores.
//!
//! Units are "points". One point is one point of a card's combat value (`SimpleAi::card_value`): a 3/3
//! for three mana is 18, a 5/5 for five is 30. Each weight below is one line in a constant.

use manabrew_agent_interface::game_view_dto::{
    CardDto, CardView, GameViewDto, StackObjectDto, TargetingIntent, ZoneKind,
};

use super::roles::{self, Roles};
use super::SimpleAi;

/// A spell on the stack must be worth at least this much to spend a counterspell on it (a 2/2 for two is 12).
pub const COUNTER_MIN: i32 = 10;
/// A permanent must be worth at least this much for a protection spell to be cast for it (a 4/4 for five is 25).
pub const PROTECT_MIN: i32 = 20;
/// An attacker worth this much does not trade into a cheaper blocker (an engine creature or a commander).
pub const KEY_PERMANENT: i32 = 40;

/// A trigger that draws, per permanent.
const ENGINE_DRAW: i32 = 25;
/// A trigger that makes tokens or counters, or fetches a card.
const ENGINE_VALUE: i32 = 12;
/// A trigger that makes each opponent lose life, per life lost each time it triggers.
const DRAIN_PER_LIFE: i32 = 15;
/// The most a permanent's triggered abilities add.
const ENGINE_CAP: i32 = 90;
/// A permanent that says its controller wins the game.
const WIN_EFFECT: i32 = 200;
/// A spell that gives its controller an extra turn.
const EXTRA_TURN: i32 = 40;
/// A commander on the battlefield.
const COMMANDER: i32 = 25;
/// A creature with an evasion role (flying is already in combat value), per point of power.
const EVASION_PER_POWER: i32 = 1;
/// A creature that can't be blocked, per point of power.
const UNBLOCKABLE_PER_POWER: i32 = 2;
/// A planeswalker, per loyalty counter.
const LOYALTY: i32 = 2;
/// A land.
const LAND: i32 = 2;
/// A non-permanent spell, per point of mana value.
const SPELL_PER_MANA: i32 = 3;
/// A non-permanent spell with a card-advantage, mana or interaction role.
const SPELL_ROLE: i32 = 6;
/// An opposing spell whose card is hidden or missing: worth countering, as before.
const UNKNOWN_SPELL: i32 = COUNTER_MIN;

/// An opponent's life below 40, per point: a hurt player is nearer to the table's attention and easier to finish.
const LIFE_BELOW_40: i32 = 1;
/// An opponent's card in hand, per card.
const HAND_CARD: i32 = 4;
/// Commander damage an opponent's commander has dealt me, per point (21 is lethal).
const COMMANDER_DAMAGE: i32 = 10;
/// An opponent whose creatures' total power is at least my life.
const LETHAL_ON_ME: i32 = 300;

fn is_creature(card: &CardDto) -> bool {
    card.types.iter().any(|ty| ty == "Creature")
}

fn has_type(card: &CardDto, name: &str) -> bool {
    card.types.iter().any(|ty| ty == name)
}

fn power(card: &CardDto) -> i32 {
    SimpleAi::stat(card.power.as_deref()).max(0)
}

fn visible_cards(view: &GameViewDto, kind: ZoneKind) -> impl Iterator<Item = &CardDto> {
    view.zones
        .iter()
        .filter(move |zone| zone.zone == kind)
        .flat_map(|zone| &zone.cards)
        .filter_map(|card| match card {
            CardView::Visible(card) => Some(card),
            CardView::Hidden { .. } => None,
        })
}

fn battlefield_of<'a>(
    view: &'a GameViewDto,
    owner: &'a str,
) -> impl Iterator<Item = &'a CardDto> + 'a {
    view.zones
        .iter()
        .filter(move |zone| zone.zone == ZoneKind::Battlefield && zone.owner_id == owner)
        .flat_map(|zone| &zone.cards)
        .filter_map(|card| match card {
            CardView::Visible(card) => Some(card),
            CardView::Hidden { .. } => None,
        })
}

fn find_card<'a>(view: &'a GameViewDto, id: &str) -> Option<&'a CardDto> {
    view.zones
        .iter()
        .flat_map(|zone| &zone.cards)
        .find_map(|card| match card {
            CardView::Visible(card) if card.id == id => Some(card),
            _ => None,
        })
}

fn life_of(view: &GameViewDto, player_id: &str) -> i32 {
    view.players
        .iter()
        .find(|player| player.id == player_id)
        .map_or(20, |player| player.life)
}

/// A commander is any card its owner has cast from the command zone.
pub fn is_commander(view: &GameViewDto, card: &CardDto) -> bool {
    view.players
        .iter()
        .any(|player| player.commander_casts.contains_key(&card.id))
        || visible_cards(view, ZoneKind::Command).any(|command| command.id == card.id)
}

/// The life lost each time a triggered line makes each opponent lose life; zero when it doesn't.
fn drain_amount(line: &str) -> i32 {
    let Some(rest) = line
        .split("each opponent loses ")
        .nth(1)
        .or_else(|| line.split("target opponent loses ").nth(1))
    else {
        return 0;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(2)
}

/// What a permanent's triggered abilities ("Whenever", "At the beginning of") add every turn it stays.
fn engine_points(text: &str) -> i32 {
    let lower = text.to_ascii_lowercase();
    let mut points = 0;
    for line in lower.lines() {
        let line = line.trim();
        if !(line.starts_with("whenever ") || line.starts_with("at the beginning of ")) {
            continue;
        }
        if line.contains("draw") {
            points += ENGINE_DRAW;
        }
        points += drain_amount(line) * DRAIN_PER_LIFE;
        if line.contains("create ")
            || line.contains("search your library")
            || line.contains("+1/+1 counter")
        {
            points += ENGINE_VALUE;
        }
    }
    points.min(ENGINE_CAP)
}

/// How much a permanent matters: combat value, evasion, engine and drain triggers, commander status.
pub fn permanent_importance(view: &GameViewDto, card: &CardDto) -> i32 {
    let text = card.text.to_ascii_lowercase();
    let roles = roles::lookup(&card.identity.name);
    let mut points = if is_creature(card) {
        SimpleAi::card_value(card)
    } else if has_type(card, "Land") {
        LAND
    } else {
        card.cmc * SPELL_PER_MANA
    };
    if has_type(card, "Planeswalker") {
        points += card.counters.get("Loyalty").copied().unwrap_or(0) as i32 * LOYALTY;
    }
    if is_creature(card) {
        let flies = SimpleAi::has_keyword(card, "Flying");
        if roles.contains(Roles::EVASIVE) && !flies {
            points += power(card) * EVASION_PER_POWER;
        }
        if text.contains("can't be blocked") || SimpleAi::has_keyword(card, "Unblockable") {
            points += power(card) * UNBLOCKABLE_PER_POWER;
        }
    }
    points += engine_points(&card.text);
    if text.contains("you win the game") {
        points += WIN_EFFECT;
    }
    if is_commander(view, card) {
        points += COMMANDER;
    }
    points
}

/// Whether the text is a spell that protects one of its controller's permanents.
pub fn is_protection(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "gains hexproof",
        "gain hexproof",
        "gains indestructible",
        "gain indestructible",
        "protection from",
        "regenerate",
    ]
    .iter()
    .any(|grant| text.contains(grant))
        && text.contains("you control")
        && !text.contains("destroy target")
        && !text.contains("exile target")
}

/// An opposing spell or ability of this stack object is aimed at the card to harm it.
fn harms(item: &StackObjectDto, card_id: &str) -> bool {
    let effect = item
        .source_ability_text
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(&item.text)
        .to_ascii_lowercase();
    let hostile_text = [
        "destroy",
        "exile",
        "damage",
        "return",
        "sacrifice",
        "-x/-x",
        "-1/-1",
        "gain control",
    ]
    .iter()
    .any(|word| effect.contains(word));
    item.targets.iter().any(|target| {
        target.id == card_id
            && match target.intent {
                Some(
                    TargetingIntent::Damage
                    | TargetingIntent::Destroy
                    | TargetingIntent::Sacrifice
                    | TargetingIntent::Exile
                    | TargetingIntent::Bounce
                    | TargetingIntent::Debuff
                    | TargetingIntent::GainControl,
                ) => true,
                Some(_) => false,
                None => hostile_text,
            }
    })
}

/// An opposing stack object that destroys or exiles every creature.
fn sweeps(item: &StackObjectDto) -> bool {
    let text = item.text.to_ascii_lowercase();
    match item.source_card.as_ref() {
        Some(card) => is_wipe(card, &card.text.to_ascii_lowercase()),
        None => text.contains("destroy all creatures") || text.contains("exile all creatures"),
    }
}

/// A permanent of `me` that an opposing stack object is aimed at to harm it, or a creature of mine
/// that an opposing wipe will take.
pub fn threatened(view: &GameViewDto, card: &CardDto, me: &str) -> bool {
    card.controller_id == me
        && view.stack.iter().any(|item| {
            item.controller_id != me
                && !item.is_casting
                && (harms(item, &card.id) || (is_creature(card) && sweeps(item)))
        })
}

/// The most important of `me`'s permanents that an opposing stack object is aimed at.
pub fn most_threatened_importance(view: &GameViewDto, me: &str) -> Option<i32> {
    battlefield_of(view, me)
        .filter(|card| threatened(view, card, me))
        .map(|card| permanent_importance(view, card))
        .max()
}

fn is_wipe(card: &CardDto, text: &str) -> bool {
    roles::lookup(&card.identity.name).contains(Roles::SWEEPER)
        || text.contains("destroy all creatures")
        || text.contains("exile all creatures")
        || (text.contains("each creature") && (text.contains("destroy") || text.contains("-x/-x")))
}

/// How much a spell on the stack matters to `me` if it resolves: a permanent counts as it would on the
/// battlefield; other spells by mana value and role, plus what they do (win, extra turn, wipe my
/// creatures, remove my permanents).
pub fn spell_importance(view: &GameViewDto, item: &StackObjectDto, me: &str) -> i32 {
    let Some(card) = item.source_card.as_ref() else {
        return UNKNOWN_SPELL;
    };
    let text = card.text.to_ascii_lowercase();
    let permanent = [
        "Creature",
        "Artifact",
        "Enchantment",
        "Planeswalker",
        "Battle",
    ]
    .iter()
    .any(|ty| has_type(card, ty));
    let mut points = if permanent {
        permanent_importance(view, card)
    } else {
        let roles = roles::lookup(&card.identity.name);
        let role = roles.intersects(
            Roles::DRAW
                | Roles::RAMP
                | Roles::REMOVAL
                | Roles::COUNTERSPELL
                | Roles::REANIMATION
                | Roles::TOKENS,
        );
        let draws = text.contains("draw ");
        card.cmc * SPELL_PER_MANA
            + if role || draws { SPELL_ROLE } else { 0 }
            + engine_points(&card.text)
    };
    if text.contains("you win the game") {
        points += WIN_EFFECT;
    }
    if text.contains("extra turn") {
        points += EXTRA_TURN;
    }
    if item.controller_id != me {
        if is_wipe(card, &text) {
            let mine: i32 = battlefield_of(view, me)
                .filter(|c| is_creature(c))
                .map(|c| permanent_importance(view, c))
                .sum();
            let theirs: i32 = battlefield_of(view, &item.controller_id)
                .filter(|c| is_creature(c))
                .map(|c| permanent_importance(view, c))
                .sum();
            points += (mine - theirs).max(0);
        }
        points += battlefield_of(view, me)
            .filter(|c| harms(item, &c.id))
            .map(|c| permanent_importance(view, c) / 2)
            .sum::<i32>();
    }
    points
}

/// How close an opponent is to winning: the importance of their permanents, their cards in hand, the
/// commander damage they have dealt me, and their missing life; a flat jump when their creatures alone
/// are lethal on me.
pub fn opponent_danger(view: &GameViewDto, opponent: &str, me: &str) -> i32 {
    let permanents: Vec<&CardDto> = battlefield_of(view, opponent)
        .filter(|c| !has_type(c, "Land"))
        .collect();
    let board: i32 = permanents
        .iter()
        .map(|card| permanent_importance(view, card))
        .sum();
    let reach: i32 = permanents
        .iter()
        .filter(|c| is_creature(c))
        .map(|c| power(c))
        .sum();
    let hand: usize = view
        .zones
        .iter()
        .filter(|zone| zone.zone == ZoneKind::Hand && zone.owner_id == opponent)
        .map(|zone| zone.count)
        .sum();
    let commander_damage: i32 =
        view.players
            .iter()
            .find(|player| player.id == me)
            .map_or(0, |player| {
                player
                    .commander_damage
                    .iter()
                    .filter(|(commander, _)| {
                        find_card(view, commander).is_some_and(|card| card.owner_id == opponent)
                    })
                    .map(|(_, damage)| *damage)
                    .sum()
            });
    let life = life_of(view, opponent).max(0);
    let mut points = board
        + hand as i32 * HAND_CARD
        + commander_damage * COMMANDER_DAMAGE
        + (40 - life).max(0) * LIFE_BELOW_40;
    if reach > 0 && reach >= life_of(view, me) {
        points += LETHAL_ON_ME;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use manabrew_agent_interface::game_view_dto::PlayerDto;
    use manabrew_agent_interface::prompt::{TargetKind, TargetRef};
    use serde_json::json;

    fn player(id: usize, life: i32) -> PlayerDto {
        serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":life,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()
    }
    fn card(id: &str, controller: usize, types: &[&str], cmc: i32, text: &str) -> CardDto {
        CardDto {
            id: id.into(),
            controller_id: format!("player-{controller}"),
            owner_id: format!("player-{controller}"),
            types: types.iter().map(|t| t.to_string()).collect(),
            cmc,
            text: text.into(),
            ..Default::default()
        }
    }
    fn creature(id: &str, controller: usize, pt: (i32, i32), text: &str) -> CardDto {
        CardDto {
            power: Some(pt.0.to_string()),
            toughness: Some(pt.1.to_string()),
            ..card(id, controller, &["Creature"], pt.0, text)
        }
    }
    /// A view with two players (`me` is player-0 at 20) and these battlefield cards, in owner order.
    fn board(opponent_life: i32, cards: Vec<CardDto>) -> GameViewDto {
        let mut view = GameViewDto::default();
        view.players = vec![player(0, 20), player(1, opponent_life)];
        for owner in 0..2 {
            let mine: Vec<_> = cards
                .iter()
                .filter(|c| c.controller_id == format!("player-{owner}"))
                .cloned()
                .map(CardView::Visible)
                .collect();
            view.zones
                .push(manabrew_agent_interface::game_view_dto::ZoneDto {
                    zone: ZoneKind::Battlefield,
                    owner_id: format!("player-{owner}"),
                    count: mine.len(),
                    cards: mine,
                });
        }
        view
    }
    fn danger(view: &GameViewDto) -> i32 {
        opponent_danger(view, "player-1", "player-0")
    }
    fn on_stack(controller: usize, source: CardDto, targets: Vec<TargetRef>) -> StackObjectDto {
        StackObjectDto {
            id: format!("stack-{}", source.id),
            controller_id: format!("player-{controller}"),
            is_spell: true,
            text: source.text.clone(),
            source_card: Some(source),
            targets,
            ..Default::default()
        }
    }
    fn aimed_at(id: &str) -> TargetRef {
        TargetRef {
            kind: TargetKind::Card,
            id: id.into(),
            intent: None,
            oracle: None,
        }
    }

    #[test]
    fn danger_rises_as_the_opponents_life_falls() {
        assert!(danger(&board(10, vec![])) > danger(&board(40, vec![])));
    }

    #[test]
    fn danger_counts_board_power() {
        let big = board(40, vec![creature("a", 1, (6, 6), "")]);
        let small = board(40, vec![creature("a", 1, (1, 1), "")]);
        assert!(danger(&big) > danger(&small) && danger(&small) > danger(&board(40, vec![])));
    }

    #[test]
    fn evasion_weighs_more_than_the_same_body_on_the_ground() {
        let ground = creature("a", 1, (3, 3), "");
        let unblockable = creature("a", 1, (3, 3), "This creature can't be blocked.");
        let flying = CardDto {
            keywords: vec!["Flying".into()],
            ..creature("a", 1, (3, 3), "")
        };
        let view = board(40, vec![]);
        assert!(permanent_importance(&view, &unblockable) > permanent_importance(&view, &ground));
        assert!(permanent_importance(&view, &flying) > permanent_importance(&view, &ground));
    }

    #[test]
    fn a_commander_on_the_battlefield_adds_danger() {
        let body = creature("a", 1, (3, 3), "");
        let plain = board(40, vec![body.clone()]);
        let mut led = board(40, vec![body]);
        led.players[1].commander_casts.insert("a".into(), 1);
        assert!(danger(&led) > danger(&plain));
    }

    #[test]
    fn commander_damage_dealt_to_me_adds_danger() {
        let commander = creature("a", 1, (3, 3), "");
        let mut view = board(40, vec![commander]);
        let before = danger(&view);
        view.players[0].commander_damage.insert("a".into(), 9);
        assert_eq!(danger(&view) - before, 9 * COMMANDER_DAMAGE);
    }

    #[test]
    fn cards_in_hand_add_danger() {
        let mut view = board(40, vec![]);
        let before = danger(&view);
        view.zones
            .push(manabrew_agent_interface::game_view_dto::ZoneDto {
                zone: ZoneKind::Hand,
                owner_id: "player-1".into(),
                count: 5,
                cards: vec![],
            });
        assert_eq!(danger(&view) - before, 5 * HAND_CARD);
    }

    #[test]
    fn lethal_power_on_me_is_a_jump() {
        let view = board(
            40,
            vec![
                creature("a", 1, (10, 10), ""),
                creature("b", 1, (10, 10), ""),
            ],
        );
        let weaker = board(40, vec![creature("a", 1, (10, 10), "")]);
        assert!(danger(&view) - danger(&weaker) >= LETHAL_ON_ME);
    }

    #[test]
    fn a_drain_or_draw_engine_outweighs_a_bigger_plain_body() {
        let view = board(40, vec![]);
        let plain = creature("a", 1, (3, 3), "");
        let engine = creature("b", 1, (1, 2), "Whenever a creature enters the battlefield under your control, draw a card and each opponent loses 1 life.");
        let drain = card(
            "c",
            1,
            &["Enchantment"],
            3,
            "At the beginning of your upkeep, each opponent loses 3 life.",
        );
        let big = creature("d", 1, (5, 5), "");
        assert!(permanent_importance(&view, &engine) > permanent_importance(&view, &plain));
        assert!(permanent_importance(&view, &drain) > permanent_importance(&view, &big));
    }

    #[test]
    fn a_commander_matters_more_than_the_same_body() {
        let body = creature("a", 0, (3, 3), "");
        let mut view = board(40, vec![body.clone()]);
        let plain = permanent_importance(&view, &body);
        view.players[0].commander_casts.insert("a".into(), 1);
        assert_eq!(permanent_importance(&view, &body) - plain, COMMANDER);
    }

    #[test]
    fn a_win_effect_and_an_extra_turn_outrank_a_big_creature() {
        let view = board(40, vec![]);
        let win = on_stack(1, card("w", 1, &["Enchantment"], 4, "At the beginning of your upkeep, if you control twenty creatures, you win the game."), vec![]);
        let turn = on_stack(
            1,
            card(
                "t",
                1,
                &["Sorcery"],
                5,
                "Take an extra turn after this one.",
            ),
            vec![],
        );
        let big = on_stack(1, creature("b", 1, (6, 6), ""), vec![]);
        assert!(
            spell_importance(&view, &win, "player-0") > spell_importance(&view, &big, "player-0")
        );
        assert!(
            spell_importance(&view, &turn, "player-0")
                > spell_importance(&view, &big, "player-0") - 10
        );
    }

    #[test]
    fn a_cheap_noncreature_spell_is_below_the_counter_threshold() {
        let view = board(40, vec![]);
        let gain = on_stack(1, card("g", 1, &["Instant"], 1, "You gain 3 life."), vec![]);
        let body = on_stack(1, creature("b", 1, (3, 3), ""), vec![]);
        assert!(spell_importance(&view, &gain, "player-0") < COUNTER_MIN);
        assert!(spell_importance(&view, &body, "player-0") >= COUNTER_MIN);
    }

    #[test]
    fn a_wipe_matters_in_proportion_to_my_creatures_it_takes() {
        let wipe = |view: &GameViewDto| {
            let item = on_stack(
                1,
                card("x", 1, &["Sorcery"], 4, "Destroy all creatures."),
                vec![],
            );
            spell_importance(view, &item, "player-0")
        };
        let none = board(40, vec![]);
        let some = board(
            40,
            vec![creature("a", 0, (4, 4), ""), creature("b", 0, (4, 4), "")],
        );
        assert!(wipe(&some) > wipe(&none));
    }

    #[test]
    fn removal_aimed_at_my_permanent_is_worth_its_target() {
        let key = creature(
            "k",
            0,
            (4, 4),
            "Whenever a creature enters the battlefield under your control, draw a card.",
        );
        let view = board(40, vec![key]);
        let kill = |target: &str| {
            let item = on_stack(
                1,
                card("r", 1, &["Instant"], 2, "Destroy target creature."),
                vec![aimed_at(target)],
            );
            spell_importance(&view, &item, "player-0")
        };
        assert!(kill("k") > kill("nothing"));
    }

    #[test]
    fn protection_needs_an_important_permanent_under_threat() {
        let key = creature(
            "k",
            0,
            (4, 4),
            "Whenever a creature enters the battlefield under your control, draw a card.",
        );
        let token = creature("t", 0, (1, 1), "");
        let mut view = board(40, vec![key, token]);
        assert_eq!(most_threatened_importance(&view, "player-0"), None);
        view.stack.push(on_stack(
            1,
            card("r", 1, &["Instant"], 2, "Destroy target creature."),
            vec![aimed_at("t")],
        ));
        assert!(most_threatened_importance(&view, "player-0").unwrap() < PROTECT_MIN);
        view.stack.push(on_stack(
            1,
            card("s", 1, &["Instant"], 2, "Destroy target creature."),
            vec![aimed_at("k")],
        ));
        assert!(most_threatened_importance(&view, "player-0").unwrap() >= PROTECT_MIN);
    }

    #[test]
    fn protection_text_is_recognized() {
        assert!(is_protection(
            "Target creature you control gains hexproof and indestructible until end of turn."
        ));
        assert!(!is_protection("Destroy target creature."));
        assert!(!is_protection("Creatures you control get +1/+1."));
    }
}
