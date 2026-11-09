//! Turn plan: how much mana to keep open for the opponents' turns. Pure functions over what the bot
//! holds and what the threat model says about the board; `SimpleAi` applies the result as a reserve.
//!
//! The rule: keep up the cheapest held answer (an instant or flash counter, removal or protection)
//! when the board calls for it, and develop otherwise. Each threshold is one constant, in the same
//! "points" as `threat.rs`.

use manabrew_agent_interface::game_view_dto::{CardDto, PlayerStatus, StepKind, ZoneKind};
use manabrew_agent_interface::prompt::{AvailableAction, AvailableActionKind};

use super::roles::Roles;
use super::{threat, SimpleAi};

/// An opponent this dangerous (see `threat::opponent_danger`) makes a held removal spell or counter
/// worth keeping mana for (a board of two 3/3s for three mana is about 36 plus hand and life).
pub const DANGER_HOLD: i32 = 60;
/// Without a dangerous opponent, a held counterspell is still kept up when nothing in hand develops the
/// board for at least this much (a 4/4 for five is 25, a 2/2 for two is 12).
pub const DEVELOP_WORTH: i32 = 20;
/// A sorcery-speed play worth at least this much is never skipped for the reserve: more than any single
/// held answer is worth (an engine creature or a commander).
pub const DEVELOP_OVERRIDE: i32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerKind {
    Counter,
    Removal,
    Protection,
}

/// An instant-speed answer the bot could cast now, with its mana cost (empty or hybrid costs are
/// never offered: they need no reserve).
#[derive(Clone, Debug)]
pub struct Answer {
    pub kind: AnswerKind,
    pub cost: String,
    pub cmc: i32,
}

/// What the board says, from the bot's side.
#[derive(Clone, Copy, Debug, Default)]
pub struct Board {
    /// The most dangerous opponent's `opponent_danger`.
    pub danger: i32,
    /// Opposing cards in hand, all opponents.
    pub opponent_hand: usize,
    /// Opposing nonland permanents.
    pub opponent_permanents: usize,
    /// I control a permanent worth at least `threat::KEY_PERMANENT`.
    pub key_permanent: bool,
    /// The best development value (permanent importance, or three points per mana) among my
    /// castable sorcery-speed cards.
    pub develop: i32,
}

/// The mana cost to keep open, or `None` to develop. Of the answers the board calls for, the
/// cheapest is kept, so the rest of the mana still develops.
pub fn reserve(answers: &[Answer], board: &Board) -> Option<String> {
    answers
        .iter()
        .filter(|answer| match answer.kind {
            AnswerKind::Removal => board.danger >= DANGER_HOLD && board.opponent_permanents > 0,
            AnswerKind::Counter => {
                board.opponent_hand > 0
                    && (board.danger >= DANGER_HOLD || board.develop < DEVELOP_WORTH)
            }
            AnswerKind::Protection => board.key_permanent && board.opponent_hand > 0,
        })
        .min_by_key(|answer| answer.cmc)
        .map(|answer| answer.cost.clone())
}

/// The pips and generic mana of a cost, leaving out `{T}`, or `None` if it has another symbol (`{X}`,
/// hybrid, phyrexian).
pub fn plain_mana(cost: &str) -> Option<String> {
    let mut out = String::new();
    for symbol in cost
        .split('{')
        .filter_map(|part| part.split('}').next())
        .filter(|s| !s.is_empty())
    {
        if symbol == "T" {
            continue;
        }
        if symbol.parse::<i32>().is_ok() || (symbol.len() == 1 && "WUBRG".contains(symbol)) {
            out.push_str(&format!("{{{symbol}}}"));
        } else {
            return None;
        }
    }
    Some(out)
}

fn is_instant_speed(card: &CardDto) -> bool {
    card.types.iter().any(|ty| ty == "Instant")
        || (SimpleAi::has_keyword(card, "Flash") && !card.types.iter().any(|ty| ty == "Creature"))
}

/// What kind of answer a card is when held at instant speed, if it is one.
fn answer_kind(card: &CardDto) -> Option<AnswerKind> {
    let text = card.text.to_ascii_lowercase();
    let roles = SimpleAi::roles(card);
    if roles.contains(Roles::COUNTERSPELL) || text.contains("counter target") {
        Some(AnswerKind::Counter)
    } else if threat::is_protection(&text) {
        Some(AnswerKind::Protection)
    } else if roles.contains(Roles::REMOVAL)
        || text.contains("destroy target")
        || text.contains("exile target")
    {
        Some(AnswerKind::Removal)
    } else {
        None
    }
}

impl SimpleAi {
    fn spell_cost(card: &CardDto) -> Option<String> {
        let cost = card
            .effective_mana_cost
            .as_deref()
            .unwrap_or(card.mana_cost.as_str());
        plain_mana(cost).filter(|cost| !cost.is_empty())
    }

    /// The instant-speed answers held: spells in hand (the engine offers a counterspell or removal
    /// only once there is something to aim at, so the offered actions would miss them) and
    /// activated abilities on offer.
    fn held_answers(&self, actions: &[AvailableAction], me: &str) -> Vec<Answer> {
        let spells = self
            .zone_cards(me, ZoneKind::Hand)
            .into_iter()
            .filter(|card| is_instant_speed(card))
            .filter_map(|card| {
                Some(Answer {
                    kind: answer_kind(card)?,
                    cost: Self::spell_cost(card)?,
                    cmc: card.cmc,
                })
            });
        let abilities = actions.iter().filter_map(|action| match &action.kind {
            AvailableActionKind::ActivateAbility(info) if !info.is_mana_ability => {
                let text = info.description.to_ascii_lowercase();
                let kind = if text.contains("counter target") {
                    AnswerKind::Counter
                } else if text.contains("destroy target") || text.contains("exile target") {
                    AnswerKind::Removal
                } else {
                    return None;
                };
                let cost = plain_mana(info.cost.as_deref()?).filter(|cost| !cost.is_empty())?;
                let cmc = cost.matches('{').count() as i32;
                Some(Answer { kind, cost, cmc })
            }
            _ => None,
        });
        spells.chain(abilities).collect()
    }

    fn turn_board(&self, actions: &[AvailableAction], me: &str) -> Option<Board> {
        let view = self.view.as_ref()?;
        let danger = view
            .players
            .iter()
            .filter(|player| player.id != me && player.status == PlayerStatus::Playing)
            .map(|player| threat::opponent_danger(view, &player.id, me))
            .max()
            .unwrap_or(0);
        let opponent_hand = view
            .zones
            .iter()
            .filter(|zone| zone.zone == ZoneKind::Hand && zone.owner_id != me)
            .map(|zone| zone.count)
            .sum();
        let develop = actions
            .iter()
            .filter_map(|action| match &action.kind {
                AvailableActionKind::Cast { card_id, .. } if !self.is_land_play(action) => {
                    self.card(card_id)
                }
                _ => None,
            })
            .filter(|card| !is_instant_speed(card))
            .map(|card| threat::permanent_importance(view, card))
            .max()
            .unwrap_or(0);
        Some(Board {
            danger,
            opponent_hand,
            opponent_permanents: self.opponent_permanents(me),
            key_permanent: self
                .battlefield(me)
                .any(|card| threat::permanent_importance(view, card) >= threat::KEY_PERMANENT),
            develop,
        })
    }

    /// The mana to keep open at this prompt, from the answers on offer and the board.
    pub(super) fn plan_reserve(&self, actions: &[AvailableAction], me: &str) -> Option<String> {
        let answers = self.held_answers(actions, me);
        if answers.is_empty() {
            return None;
        }
        let reserve = reserve(&answers, &self.turn_board(actions, me)?)?;
        // A reserve the open mana cannot pay anyway is not one to keep.
        self.cost_affordable(&reserve, me).then_some(reserve)
    }

    /// The opponent whose turn is last before mine is the one whose end step is the last chance to
    /// spend the reserve.
    pub(super) fn last_opponent_turn(&self, me: &str) -> bool {
        let Some(view) = self.view.as_ref() else {
            return false;
        };
        let alive: Vec<&str> = view
            .players
            .iter()
            .filter(|player| player.status == PlayerStatus::Playing)
            .map(|player| player.id.as_str())
            .collect();
        let Some(active) = alive.iter().position(|id| *id == view.active_player_id) else {
            return false;
        };
        alive[(active + 1) % alive.len()] == me
    }

    /// The end step of the last opponent's turn: the last chance to spend a reserve that went unused.
    pub(super) fn reserve_spending_window(&self, me: &str) -> bool {
        self.view.as_ref().is_some_and(|view| {
            view.active_player_id != me && view.step == StepKind::EndOfTurn && self.last_opponent_turn(me)
        })
    }

    /// A protection spell with independent lasting value needs its end-step opportunity even
    /// before there is a key permanent to protect (and therefore no protection reserve). Otherwise
    /// the pass-until shortcut can make the spending rule unreachable for the entire game.
    pub(super) fn holds_value_protection(&self, me: &str) -> bool {
        self.zone_cards(me, ZoneKind::Hand).into_iter().any(|card| {
            is_instant_speed(card) && threat::is_protection(&card.text)
                && threat::has_lasting_effect(&card.text) && self.affordable(card, me)
        })
    }

    /// Whether a spell is worth more than keeping the mana: an answer itself, a mana source, a
    /// commander, a win, or a permanent worth `DEVELOP_OVERRIDE`.
    fn beats_reserve(&self, card: &CardDto, me: &str) -> bool {
        let Some(view) = self.view.as_ref() else {
            return true;
        };
        let roles = Self::roles(card);
        let text = card.text.to_ascii_lowercase();
        answer_kind(card).is_some()
            || roles.intersects(Roles::SWEEPER | Roles::COUNTERSPELL | Roles::REMOVAL)
            || Self::is_mana_source(card)
            || self.card_zone(&card.id) == Some(ZoneKind::Command)
            || text.contains("you win the game")
            || self.mass_life_loss_wins(card, me)
            || threat::permanent_importance(view, card) >= DEVELOP_OVERRIDE
    }

    /// Whether this action would spend mana the plan keeps open. Casts and activations that are
    /// themselves the answer or clearly worth more are not held back, nor is anything while an
    /// opposing spell is on the stack (answering it is the reserve's job) or at the end of the last
    /// opponent's turn.
    pub(super) fn dips_into_reserve(&self, action: &AvailableAction, me: &str) -> bool {
        let (Some(reserve), Some(view)) = (self.reserve.as_deref(), self.view.as_ref()) else {
            return false;
        };
        if view.stack.iter().any(|item| item.controller_id != me) {
            return false;
        }
        let cost = match &action.kind {
            AvailableActionKind::Cast { card_id, .. } if !self.is_land_play(action) => {
                let Some(card) = self.card(card_id) else {
                    return false;
                };
                if self.beats_reserve(card, me) {
                    return false;
                }
                // A cost with {X} or a hybrid symbol counts as its mana value, colorless.
                Self::spell_cost(card).unwrap_or_else(|| {
                    format!("{{{}}}", card.cmc + card.commander_tax.unwrap_or(0))
                })
            }
            AvailableActionKind::ActivateAbility(info) if !info.is_mana_ability => {
                let Some(cost) = info.cost.as_deref().and_then(plain_mana) else {
                    return false;
                };
                cost
            }
            _ => return false,
        };
        if cost.is_empty() || !self.cost_affordable(reserve, me) {
            return false;
        }
        let spending_window = self.reserve_spending_window(me);
        !spending_window && !self.cost_affordable(&format!("{reserve}{cost}"), me)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(kind: AnswerKind, cost: &str, cmc: i32) -> Answer {
        Answer {
            kind,
            cost: cost.into(),
            cmc,
        }
    }

    fn board(danger: i32, develop: i32) -> Board {
        Board {
            danger,
            opponent_hand: 3,
            opponent_permanents: 2,
            key_permanent: false,
            develop,
        }
    }

    #[test]
    fn removal_is_kept_up_only_against_a_dangerous_board() {
        let held = [answer(AnswerKind::Removal, "{2}{W}", 3)];
        assert_eq!(
            reserve(&held, &board(DANGER_HOLD, 10)).as_deref(),
            Some("{2}{W}")
        );
        assert_eq!(reserve(&held, &board(DANGER_HOLD - 1, 10)), None);
    }

    #[test]
    fn removal_needs_something_to_remove() {
        let held = [answer(AnswerKind::Removal, "{2}{W}", 3)];
        let empty = Board {
            opponent_permanents: 0,
            ..board(DANGER_HOLD, 10)
        };
        assert_eq!(reserve(&held, &empty), None);
    }

    #[test]
    fn a_counter_is_kept_up_when_nothing_develops_or_the_board_is_dangerous() {
        let held = [answer(AnswerKind::Counter, "{U}{U}", 2)];
        assert!(reserve(&held, &board(0, DEVELOP_WORTH - 1)).is_some());
        assert!(reserve(&held, &board(DANGER_HOLD, 40)).is_some());
        assert_eq!(reserve(&held, &board(0, DEVELOP_WORTH)), None);
    }

    #[test]
    fn a_counter_needs_an_opponent_with_cards() {
        let held = [answer(AnswerKind::Counter, "{U}{U}", 2)];
        let dry = Board {
            opponent_hand: 0,
            ..board(DANGER_HOLD, 0)
        };
        assert_eq!(reserve(&held, &dry), None);
    }

    #[test]
    fn protection_is_kept_up_for_a_key_permanent_only() {
        let held = [answer(AnswerKind::Protection, "{G}", 1)];
        assert_eq!(reserve(&held, &board(0, 0)), None);
        let key = Board {
            key_permanent: true,
            ..board(0, 0)
        };
        assert_eq!(reserve(&held, &key).as_deref(), Some("{G}"));
    }

    #[test]
    fn the_cheapest_wanted_answer_is_kept() {
        let held = [
            answer(AnswerKind::Removal, "{3}{B}", 4),
            answer(AnswerKind::Counter, "{U}{U}", 2),
            answer(AnswerKind::Protection, "{G}", 1),
        ];
        // Protection is not wanted without a key permanent, so the counter is the cheapest wanted.
        assert_eq!(
            reserve(&held, &board(DANGER_HOLD, 40)).as_deref(),
            Some("{U}{U}")
        );
    }

    #[test]
    fn plain_mana_keeps_generic_and_pips_and_rejects_the_rest() {
        assert_eq!(plain_mana("{2}{W}{W}").as_deref(), Some("{2}{W}{W}"));
        assert_eq!(plain_mana("{1}{G}, {T}").as_deref(), Some("{1}{G}"));
        assert_eq!(plain_mana("{X}{R}"), None);
        assert_eq!(plain_mana("{G/U}"), None);
    }

    use manabrew_agent_interface::game_view_dto::{CardView, GameViewDto, PlayerDto, ZoneDto};
    use serde_json::json;

    const ANSWER: &str = "Destroy target creature an opponent controls.";

    fn player(id: usize) -> PlayerDto {
        serde_json::from_value(json!({"id":format!("player-{id}"),"name":"p","status":"playing","isHuman":false,"life":20,"counters":{},"manaPool":{},"commanderDamage":{},"hasCityBlessing":false,"hasEnduringStory":false,"ringLevel":0,"speed":0})).unwrap()
    }
    fn card(id: &str, owner: usize, types: &[&str], cost: &str, cmc: i32, text: &str) -> CardDto {
        CardDto {
            id: id.into(),
            controller_id: format!("player-{owner}"),
            owner_id: format!("player-{owner}"),
            types: types.iter().map(|t| t.to_string()).collect(),
            mana_cost: cost.into(),
            cmc,
            text: text.into(),
            ..Default::default()
        }
    }
    fn land(id: &str, subtype: &str) -> CardDto {
        CardDto {
            subtypes: vec![subtype.into()],
            ..card(id, 0, &["Land"], "", 0, "")
        }
    }
    fn fatty(id: &str) -> CardDto {
        CardDto {
            power: Some("6".into()),
            toughness: Some("6".into()),
            ..card(id, 1, &["Creature"], "{2}{G}", 3, "")
        }
    }

    /// Four players, `me` is player-0 with these lands and hand; player-1 has `fatties` big creatures
    /// and seven cards in hand.
    fn table(
        active: usize,
        step: StepKind,
        lands: Vec<CardDto>,
        hand: Vec<CardDto>,
        fatties: usize,
    ) -> SimpleAi {
        let mut view = GameViewDto::default();
        view.active_player_id = format!("player-{active}");
        view.step = step;
        view.players = (0..4).map(player).collect();
        let zone = |kind, owner: usize, cards: Vec<CardDto>, count| ZoneDto {
            zone: kind,
            owner_id: format!("player-{owner}"),
            count,
            cards: cards.into_iter().map(CardView::Visible).collect(),
        };
        let held = hand.len();
        view.zones = vec![
            zone(ZoneKind::Battlefield, 0, lands, 0),
            zone(ZoneKind::Hand, 0, hand, held),
            zone(
                ZoneKind::Battlefield,
                1,
                (0..fatties)
                    .map(|i| fatty(&format!("card-{}", 50 + i)))
                    .collect(),
                0,
            ),
            zone(ZoneKind::Hand, 1, vec![], 7),
        ];
        let mut bot = SimpleAi::default();
        crate::agent::BotAgent::observe(&mut bot, view);
        bot
    }
    fn cast(card: &str) -> AvailableAction {
        serde_json::from_value(json!({"type":"cast","id":format!("cast:{card}:normal"),"cardId":card,"mode":{"type":"normal"},"label":"Cast normally"})).unwrap()
    }
    fn plains(count: usize) -> Vec<CardDto> {
        (0..count)
            .map(|i| land(&format!("card-{i}"), "Plains"))
            .collect()
    }
    fn answer_card() -> CardDto {
        card("card-8", 0, &["Instant"], "{2}{W}", 3, ANSWER)
    }
    fn planned(bot: &mut SimpleAi, offered: &[AvailableAction]) {
        bot.reserve = bot.plan_reserve(offered, "player-0");
    }

    #[test]
    fn a_dangerous_board_and_a_held_answer_make_a_reserve() {
        let mut bot = table(0, StepKind::Main1, plains(4), vec![answer_card()], 3);
        planned(&mut bot, &[]);
        assert_eq!(bot.reserve.as_deref(), Some("{2}{W}"));
        let mut quiet = table(0, StepKind::Main1, plains(4), vec![answer_card()], 0);
        planned(&mut quiet, &[]);
        assert_eq!(quiet.reserve, None);
    }

    #[test]
    fn no_reserve_when_the_open_mana_cannot_pay_it() {
        let mut bot = table(0, StepKind::Main1, plains(2), vec![answer_card()], 3);
        planned(&mut bot, &[]);
        assert_eq!(bot.reserve, None);
    }

    #[test]
    fn a_cast_that_would_dip_into_the_reserve_is_skipped_and_one_that_fits_is_not() {
        let creature = card("card-9", 0, &["Creature"], "{1}{W}", 2, "");
        let mut bot = table(
            0,
            StepKind::Main1,
            plains(5),
            vec![answer_card(), creature],
            3,
        );
        planned(&mut bot, &[]);
        // Five lands, three kept: the two-mana creature fits, a second one would not.
        assert!(!bot.dips_into_reserve(&cast("card-9"), "player-0"));
        let mut four = table(
            0,
            StepKind::Main1,
            plains(4),
            vec![
                answer_card(),
                card("card-9", 0, &["Creature"], "{1}{W}", 2, ""),
            ],
            3,
        );
        planned(&mut four, &[]);
        assert!(four.dips_into_reserve(&cast("card-9"), "player-0"));
    }

    #[test]
    fn a_land_drop_and_the_answer_itself_are_never_held_back() {
        let mut bot = table(
            0,
            StepKind::Main1,
            plains(4),
            vec![answer_card(), land("card-7", "Plains")],
            3,
        );
        planned(&mut bot, &[]);
        let land_play: AvailableAction = serde_json::from_value(json!({"type":"cast","id":"cast:card-7:normal","cardId":"card-7","mode":{"type":"normal"},"label":"Play Plains"})).unwrap();
        assert!(!bot.dips_into_reserve(&land_play, "player-0"));
        assert!(!bot.dips_into_reserve(&cast("card-8"), "player-0"));
    }

    #[test]
    fn a_mana_rock_and_a_game_winning_spell_beat_the_reserve() {
        let rock = card("card-6", 0, &["Artifact"], "{2}", 2, "{T}: Add {C}{C}.");
        let win = card(
            "card-5",
            0,
            &["Enchantment"],
            "{2}{W}",
            3,
            "At the beginning of your upkeep, you win the game.",
        );
        let mut bot = table(
            0,
            StepKind::Main1,
            plains(4),
            vec![answer_card(), rock, win],
            3,
        );
        planned(&mut bot, &[]);
        assert!(!bot.dips_into_reserve(&cast("card-6"), "player-0"));
        assert!(!bot.dips_into_reserve(&cast("card-5"), "player-0"));
    }

    #[test]
    fn the_reserve_keeps_its_colors() {
        let lands = vec![
            land("l0", "Island"),
            land("l1", "Island"),
            land("l2", "Plains"),
            land("l3", "Plains"),
        ];
        let counter = card(
            "card-8",
            0,
            &["Instant"],
            "{U}{U}",
            2,
            "Counter target spell.",
        );
        let white = card("card-9", 0, &["Creature"], "{W}{W}", 2, "");
        let blue = card("card-4", 0, &["Creature"], "{1}{U}", 2, "");
        let mut bot = table(0, StepKind::Main1, lands, vec![counter, white, blue], 0);
        planned(&mut bot, &[]);
        assert_eq!(bot.reserve.as_deref(), Some("{U}{U}"));
        // Two blue sources are kept for the counter: the white spell fits, the blue one does not.
        assert!(!bot.dips_into_reserve(&cast("card-9"), "player-0"));
        assert!(bot.dips_into_reserve(&cast("card-4"), "player-0"));
    }

    #[test]
    fn a_non_answer_instant_waits_for_the_last_opponents_end_step() {
        let draw = card("card-9", 0, &["Instant"], "{1}{W}", 2, "Draw two cards.");
        let hand = vec![answer_card(), draw];
        let mut early = table(1, StepKind::EndOfTurn, plains(4), hand.clone(), 3);
        planned(&mut early, &[]);
        assert!(early.dips_into_reserve(&cast("card-9"), "player-0"));
        // Player-3 takes the last turn before player-0's.
        let mut last = table(3, StepKind::EndOfTurn, plains(4), hand.clone(), 3);
        planned(&mut last, &[]);
        assert!(!last.dips_into_reserve(&cast("card-9"), "player-0"));
        let mut combat = table(3, StepKind::CombatDeclareAttackers, plains(4), hand, 3);
        planned(&mut combat, &[]);
        assert!(combat.dips_into_reserve(&cast("card-9"), "player-0"));
    }

    #[test]
    fn nothing_is_held_back_while_an_opposing_spell_is_on_the_stack() {
        let creature = card("card-9", 0, &["Creature"], "{1}{W}", 2, "");
        let mut bot = table(
            1,
            StepKind::Main1,
            plains(4),
            vec![answer_card(), creature],
            3,
        );
        planned(&mut bot, &[]);
        assert!(bot.dips_into_reserve(&cast("card-9"), "player-0"));
        let mut view = bot.view.clone().unwrap();
        view.stack
            .push(manabrew_agent_interface::game_view_dto::StackObjectDto {
                id: "s".into(),
                controller_id: "player-1".into(),
                is_spell: true,
                ..Default::default()
            });
        crate::agent::BotAgent::observe(&mut bot, view);
        assert!(!bot.dips_into_reserve(&cast("card-9"), "player-0"));
    }

    #[test]
    fn a_mana_activation_that_dips_is_skipped() {
        let mut bot = table(0, StepKind::Main1, plains(4), vec![answer_card()], 3);
        planned(&mut bot, &[]);
        let ability: AvailableAction = serde_json::from_value(json!({"type":"activateAbility","id":"ability:card-1:0","cardId":"card-1","abilityIndex":0,"description":"Draw a card.","isManaAbility":false,"cost":"{2}"})).unwrap();
        assert!(bot.dips_into_reserve(&ability, "player-0"));
        let free: AvailableAction = serde_json::from_value(json!({"type":"activateAbility","id":"ability:card-1:1","cardId":"card-1","abilityIndex":1,"description":"Draw a card.","isManaAbility":false,"cost":"{T}"})).unwrap();
        assert!(!bot.dips_into_reserve(&free, "player-0"));
    }
}
