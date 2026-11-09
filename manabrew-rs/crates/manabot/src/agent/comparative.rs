//! Bounded, public-observation policies. These do not execute engine branches.
//! Effect scores are conditional on the declared instruction resolving unchanged;
//! they are not a probability or a guarantee. Engine effect RNG is thread-local,
//! so cloning game state would not isolate speculative execution.
use super::{ActionFacts, BotAgent, EffectFact, PaymentFacts, ResourceFacts, SimpleAi};
use manabrew_protocol::{
    game::{CardView, GameViewDto, StepKind, ZoneKind},
    prompts::{
        choose_action::ChooseActionOutput,
        common::{AvailableActionKind, PlayCardMode},
        PromptInput, PromptOutput,
    },
    transport::AgentPrompt,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
struct ProjectedPosition {
    mana: i32,
    capacity: i32,
    hand: i32,
    own_board: i32,
    enemy_board: i32,
}
impl ProjectedPosition {
    fn value(&self) -> (i32, Value) {
        let board = self.own_board * 2 - self.enemy_board * 2;
        let access = self.hand.min(5) * 2;
        let mana = self.mana + self.capacity * 2;
        let exposure = (self.enemy_board - self.own_board).max(0) * -2;
        (
            board + access + mana + exposure,
            json!({"board":board,"access":access,"mana":mana,"exposure":exposure}),
        )
    }
}
#[derive(Clone, Copy)]
pub enum Variant {
    Strategic,
    Consequence,
}
pub struct Comparative {
    fallback: SimpleAi,
    variant: Variant,
    view: Option<GameViewDto>,
    fresh_view: bool,
    state: String,
    attempted: std::collections::BTreeSet<String>,
    facts: Option<Vec<ActionFacts>>,
    resources: Option<ResourceFacts>,
    payments: Option<Vec<PaymentFacts>>,
    record: Option<Value>,
}
impl Comparative {
    pub fn new(variant: Variant) -> Self {
        Self {
            fallback: SimpleAi::default(),
            variant,
            view: None,
            fresh_view: false,
            state: String::new(),
            attempted: Default::default(),
            facts: None,
            resources: None,
            payments: None,
            record: None,
        }
    }

    fn evaluate(
        &self,
        prompt: &AgentPrompt,
        facts: &[ActionFacts],
        resources: &ResourceFacts,
        payments: &[PaymentFacts],
        alternatives: &mut Vec<Value>,
    ) -> Result<(PromptOutput, Value), String> {
        let PromptInput::ChooseAction(input) = &prompt.input else {
            return Err("unsupported prompt".into());
        };
        // The entire menu must have unambiguous identities, including actions
        // this evaluator does not score. An empty ID collides with pass below.
        let mut seen = std::collections::BTreeSet::new();
        for action in &input.actions {
            if action.id.is_empty() {
                return Err("empty action identity".into());
            }
            if !seen.insert(&action.id) {
                return Err("duplicate action identity".into());
            }
        }
        let view = self.view.as_ref().ok_or("missing observation")?;
        let me = &prompt.deciding_player_id;
        if view.active_player_id != *me
            || !matches!(view.step, StepKind::Main1 | StepKind::Main2)
            || !view.stack.is_empty()
        {
            return Err("outside supported main-phase empty-stack subset".into());
        }
        if !(0..=1000).contains(&resources.available) || !(0..=1000).contains(&resources.capacity) {
            return Err("invalid resource totals".into());
        }
        let mut own = 0;
        let mut enemy = 0;
        let mut hand = None;
        let mut library = None;
        let mut hand_cards = Vec::new();
        for zone in &view.zones {
            if zone.zone == ZoneKind::Hand && zone.owner_id == *me {
                if hand.is_some() || zone.count != zone.cards.len() || zone.count > 1000 {
                    return Err("incomplete hand".into());
                }
                hand = Some(zone.count as i32);
                hand_cards.extend(zone.cards.iter());
            }
            if zone.zone == ZoneKind::Library && zone.owner_id == *me {
                if library.is_some() || zone.count > 1000 {
                    return Err("invalid library count".into());
                }
                library = Some(zone.count as i32);
            }
            if zone.zone != ZoneKind::Battlefield {
                continue;
            }
            if zone.count != zone.cards.len() || zone.count > 1000 {
                return Err("incomplete battlefield".into());
            }
            for c in &zone.cards {
                let CardView::Visible(c) = c else {
                    return Err("hidden battlefield card".into());
                };
                if c.phased_out || !c.types.iter().any(|t| t == "Creature") {
                    continue;
                }
                let power = c
                    .power
                    .as_deref()
                    .and_then(|p| p.parse::<i32>().ok())
                    .filter(|p| (-100..=100).contains(p))
                    .ok_or("unknown battlefield power")?;
                // Board is a crude nonnegative attack-pressure measure, not literal power.
                if c.controller_id == *me {
                    own += power.max(0);
                } else {
                    enemy += power.max(0);
                }
            }
        }
        let hand = hand.ok_or("missing hand")?;
        let before = ProjectedPosition {
            mana: resources.available,
            capacity: resources.capacity,
            hand,
            own_board: own,
            enemy_board: enemy,
        };
        let (before_value, before_components) = before.value();
        let pass_score = if matches!(self.variant, Variant::Consequence) {
            before_value
        } else {
            0
        };
        *alternatives = vec![
            json!({"action":"pass","cost":0,"score":pass_score,"before":before,"after":before,"components":if matches!(self.variant, Variant::Consequence) { before_components.clone() } else { json!({"retain":0}) }}),
        ];
        let mut ranked = vec![(pass_score, String::new())];
        for action in &input.actions {
            if matches!(
                &action.kind,
                AvailableActionKind::UndoMana { .. }
                    | AvailableActionKind::ActivateAbility(
                        manabrew_protocol::prompts::common::ActivatableAbilityInfo {
                            is_mana_ability: true,
                            ..
                        }
                    )
            ) {
                continue;
            }
            if self.attempted.contains(&action.id) {
                alternatives.push(
                    json!({"action":action.id,"skipped":"already attempted in this observation"}),
                );
                continue;
            }
            let AvailableActionKind::Cast {
                card_id,
                mode: PlayCardMode::Normal,
                face_index: None | Some(0),
                ..
            } = &action.kind
            else {
                return Err("unsupported action kind or mode".into());
            };
            if !hand_cards
                .iter()
                .any(|c| matches!(c, CardView::Visible(c) if &c.id == card_id && c.owner_id == *me))
            {
                return Err("cast card not in visible hand".into());
            }
            let mut matching = facts.iter().filter(|f| f.action_id == action.id);
            let f = matching.next().ok_or("missing action facts")?;
            if matching.next().is_some() || f.mode != "normal" || !f.unknown.is_empty() {
                return Err("ambiguous or unknown action facts".into());
            }
            let effect = f.effect.as_ref().ok_or("missing effect")?;
            let cost_text = f.mana_cost.as_deref().ok_or("missing cost")?;
            let cost = parse_cost(cost_text).ok_or("unrepresentable mana cost")?;
            let mut matching = payments.iter().filter(|p| p.action_id == action.id);
            let payment = matching.next().ok_or("missing verified payment")?;
            if matching.next().is_some()
                || payment.spent != cost
                || payment.remaining < 0
                || payment.remaining.checked_add(payment.spent) != Some(resources.available)
            {
                return Err("invalid payment accounting".into());
            }
            let scope = f.target_scope.as_deref();
            let mut after = before.clone();
            after.mana = payment.remaining;
            after.hand -= 1;
            let mut strategic = serde_json::Map::new();
            let threat = (enemy - own).max(0);
            let score = match effect {
                EffectFact::Creature { power, toughness }
                    if scope == Some("self permanent")
                        && (-100..=100).contains(power)
                        && (-100..=100).contains(toughness) =>
                {
                    let survives = *toughness > 0;
                    let board_gain = if survives { (*power).max(0) } else { 0 };
                    after.own_board += board_gain;
                    strategic.insert("development".into(), json!(board_gain * 2));
                    strategic.insert(
                        "defense".into(),
                        json!(if survives && threat > 0 { 3 } else { 0 }),
                    );
                    strategic.insert("cost".into(), json!(-cost));
                    strategic.insert("card".into(), json!(-2));
                    board_gain * 2 + if survives && threat > 0 { 3 } else { 0 } - cost - 2
                }
                EffectFact::Draw { cards }
                    if scope == Some("controller") && (1..=100).contains(cards) =>
                {
                    let count = library.ok_or("missing library count")?;
                    if *cards > count {
                        return Err("draw exceeds library count".into());
                    }
                    after.hand += cards;
                    let gain = cards * 3;
                    let need = if hand <= 3 { 3 } else { 0 };
                    let danger = if threat > 5 { -3 } else { 0 };
                    strategic.insert("draw".into(), json!(gain));
                    strategic.insert("need".into(), json!(need));
                    strategic.insert("danger".into(), json!(danger));
                    strategic.insert("cost".into(), json!(-cost));
                    strategic.insert("card".into(), json!(-2));
                    gain + need + danger - cost - 2
                }
                EffectFact::ManaPermanent { produced, creature }
                    if scope == Some("self permanent")
                        && produced.len() == 1
                        && matches!(produced[0].as_str(), "W" | "U" | "B" | "R" | "G" | "C")
                        && !creature =>
                {
                    after.capacity += 1; // Future turns only; no immediate mana is assumed.
                    let investment = if resources.capacity < 5 { 6 } else { 2 };
                    strategic.insert("future_capacity".into(), json!(investment));
                    strategic.insert("cost".into(), json!(-cost));
                    strategic.insert("card".into(), json!(-2));
                    investment - cost - 2
                }
                EffectFact::DestroyCreatures => {
                    return Err("wipe outcome is not established by public facts".into())
                }
                _ => return Err("unsupported effect or target scope".into()),
            };
            let (projected, components) = after.value();
            let (score, components) = if matches!(self.variant, Variant::Consequence) {
                (projected, components)
            } else {
                (score, Value::Object(strategic))
            };
            alternatives.push(json!({"action":action.id,"cost":cost_text,"payment":payment,"effect":effect,"score":score,"before":before,"after":after,"components":components,"resolution_uncertain":f.resolution_uncertain,"assumption":"conditional on instruction resolving unchanged"}));
            ranked.push((score, action.id.clone()));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let chosen = &ranked[0].1;
        let output = if chosen.is_empty() {
            PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                until: None,
                exhaust_stack: false,
            })
        } else {
            PromptOutput::ChooseAction(ChooseActionOutput::Act {
                action_id: chosen.clone(),
            })
        };
        Ok((
            output,
            json!({"fallback":false,"alternatives":alternatives,"chosen":if chosen.is_empty() {"pass"} else {chosen},"before_value":before_value,"before_components":before_components}),
        ))
    }
}

// Accept only fixed generic and single-color symbols. No hybrid, variable, life,
// or non-mana costs. The separate host payment attests actual color legality.
fn parse_cost(text: &str) -> Option<i32> {
    let mut rest = text.trim();
    if rest.is_empty() {
        return Some(0);
    }
    let mut total: i32 = 0;
    while !rest.is_empty() {
        rest = rest.strip_prefix('{')?;
        let (symbol, tail) = rest.split_once('}')?;
        let n = if matches!(symbol, "W" | "U" | "B" | "R" | "G" | "C") {
            1
        } else {
            symbol
                .parse::<i32>()
                .ok()
                .filter(|n| (0..=100).contains(n))?
        };
        total = total.checked_add(n)?;
        if total > 100 {
            return None;
        }
        rest = tail.trim_start();
    }
    Some(total)
}
impl BotAgent for Comparative {
    fn observe(&mut self, view: GameViewDto) {
        let state = serde_json::to_string(&view).unwrap_or_default();
        if state != self.state {
            self.attempted.clear();
            self.state = state;
        }
        self.facts = None;
        self.resources = None;
        self.payments = None;
        self.fresh_view = true;
        self.fallback.observe(view.clone());
        self.view = Some(view);
    }
    fn observe_action_facts(&mut self, facts: &[ActionFacts]) {
        self.facts = Some(facts.to_vec());
    }
    fn observe_resources(&mut self, facts: Option<ResourceFacts>) {
        self.resources = facts;
    }
    fn observe_payments(&mut self, facts: &[PaymentFacts]) {
        self.payments = Some(facts.to_vec());
    }
    fn take_decision_record(&mut self) -> Option<Value> {
        self.record.take()
    }
    fn decide(&mut self, prompt: AgentPrompt) -> Option<PromptOutput> {
        self.record = None;
        let facts = self.facts.take();
        let resources = self.resources.take();
        let payments = self.payments.take();
        let mut alternatives = Vec::new();
        let fresh = std::mem::replace(&mut self.fresh_view, false);
        let result = match (
            fresh,
            facts.as_deref(),
            resources.as_ref(),
            payments.as_deref(),
        ) {
            (true, Some(f), Some(r), Some(p)) => self.evaluate(&prompt, f, r, p, &mut alternatives),
            _ => Err("missing or stale decision facts, resources, or verified payments".into()),
        };
        match result {
            Ok((output, mut record)) => {
                if let PromptOutput::ChooseAction(ChooseActionOutput::Act { action_id }) = &output {
                    self.attempted.insert(action_id.clone());
                }
                record["output"] = json!(output);
                self.record = Some(record);
                Some(output)
            }
            Err(reason) => {
                let output = self
                    .fallback
                    .decide(prompt)
                    .unwrap_or(PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                        until: None,
                        exhaust_stack: false,
                    }));
                self.record = Some(
                    json!({"fallback":true,"reason":reason,"alternatives":alternatives,"facts":facts,"resources":resources,"payments":payments,"output":output}),
                );
                Some(output)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentKind, BotResponder};
    use manabrew_agent_interface::agent_impl::Responder;
    use manabrew_protocol::game::{CardDto, ZoneDto};

    fn view(me: &str) -> GameViewDto {
        let mut view = GameViewDto::default();
        view.active_player_id = me.into();
        view.step = StepKind::Main1;
        let cards = ["a", "b", "c"]
            .into_iter()
            .map(|id| {
                CardView::Visible(CardDto {
                    id: id.into(),
                    owner_id: me.into(),
                    controller_id: me.into(),
                    ..Default::default()
                })
            })
            .collect();
        view.zones = vec![
            ZoneDto {
                zone: ZoneKind::Hand,
                owner_id: me.into(),
                cards,
                count: 3,
            },
            ZoneDto {
                zone: ZoneKind::Library,
                owner_id: me.into(),
                cards: vec![],
                count: 10,
            },
        ];
        view
    }
    fn prompt(me: &str, order: &[&str]) -> AgentPrompt {
        let actions: Vec<Value> = order.iter().map(|id| json!({"type":"cast","id":id,"cardId":id,"mode":{"type":"normal"},"label":"cast"})).collect();
        serde_json::from_value(json!({"promptId":1,"decidingPlayerId":me,"input":{"type":"chooseAction","actions":actions}})).unwrap()
    }
    fn facts() -> Vec<ActionFacts> {
        vec![
            ActionFacts {
                action_id: "a".into(),
                mode: "normal".into(),
                mana_cost: Some("{2}".into()),
                target_scope: Some("self permanent".into()),
                effect: Some(EffectFact::Creature {
                    power: 4,
                    toughness: 4,
                }),
                unknown: vec![],
                resolution_uncertain: true,
            },
            ActionFacts {
                action_id: "b".into(),
                mode: "normal".into(),
                mana_cost: Some("{1}".into()),
                target_scope: Some("controller".into()),
                effect: Some(EffectFact::Draw { cards: 2 }),
                unknown: vec![],
                resolution_uncertain: true,
            },
            ActionFacts {
                action_id: "c".into(),
                mode: "normal".into(),
                mana_cost: Some("{1}".into()),
                target_scope: Some("self permanent".into()),
                effect: Some(EffectFact::ManaPermanent {
                    produced: vec!["G".into()],
                    creature: false,
                }),
                unknown: vec![],
                resolution_uncertain: true,
            },
        ]
    }
    fn payments() -> Vec<PaymentFacts> {
        [("a", 2), ("b", 1), ("c", 1)]
            .into_iter()
            .map(|(id, spent)| PaymentFacts {
                action_id: id.into(),
                spent,
                remaining: 4 - spent,
            })
            .collect()
    }
    fn ready(bot: &mut BotResponder, me: &str, facts: &[ActionFacts]) {
        bot.observe(view(me));
        bot.observe_resources(Some(ResourceFacts {
            available: 4,
            capacity: 4,
        }));
        bot.observe_action_facts(facts);
        bot.observe_payments(&payments());
    }
    fn run(bot: &mut BotResponder, p: AgentPrompt) -> (Value, Value) {
        let output = serde_json::to_value(bot.respond(p)).unwrap();
        let record = bot.take_decision_record().expect("current record");
        assert_eq!(record["output"], output["action"]);
        (output, record)
    }
    #[test]
    fn production_selection_evaluates_and_records_both_policies() {
        assert_eq!(AgentKind::default(), AgentKind::Simple);
        for kind in [AgentKind::Strategic, AgentKind::Consequence] {
            let mut bot = BotResponder::new(kind.build());
            ready(&mut bot, "player-0", &facts());
            let (output, record) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
            assert_eq!(record["fallback"], false);
            assert_eq!(record["alternatives"].as_array().unwrap().len(), 4);
            assert_eq!(record["chosen"], output["action"]["output"]["actionId"]);
            let a = &record["alternatives"][1];
            assert_eq!(a["before"]["mana"], 4);
            assert_eq!(a["after"]["mana"], 2);
            assert_eq!(a["after"]["hand"], 2);
            let parts = a["components"].as_object().unwrap();
            assert_eq!(
                parts.values().map(|v| v.as_i64().unwrap()).sum::<i64>(),
                a["score"].as_i64().unwrap()
            );
            assert_eq!(record["alternatives"][3]["after"]["mana"], 3);
            assert_eq!(record["alternatives"][3]["after"]["capacity"], 5);
            assert!(bot.take_decision_record().is_none());
        }
    }
    #[test]
    fn equivalent_seats_and_reordered_menu_select_same_action() {
        for kind in [AgentKind::Strategic, AgentKind::Consequence] {
            let mut a = BotResponder::new(kind.build());
            let mut b = BotResponder::new(kind.build());
            ready(&mut a, "player-0", &facts());
            ready(&mut b, "player-2", &facts());
            let (_, x) = run(&mut a, prompt("player-0", &["a", "b", "c"]));
            let (_, y) = run(&mut b, prompt("player-2", &["c", "a", "b"]));
            assert_eq!(x["chosen"], y["chosen"]);
        }
    }
    #[test]
    fn invalid_facts_and_context_fall_back_with_current_output() {
        for reason_case in 0..8 {
            let mut bot = BotResponder::new(AgentKind::Consequence.build());
            let mut f = facts();
            let mut v = view("player-0");
            let mut p = payments();
            match reason_case {
                0 => f[0].unknown.push("unknown".into()),
                1 => f[0].mana_cost = Some("{X}".into()),
                2 => f[0].target_scope = Some("opponent".into()),
                3 => {
                    f.remove(0);
                }
                4 => p[0].remaining = 4,
                5 => {
                    if let Some(z) = v.zones.iter_mut().find(|z| z.zone == ZoneKind::Library) {
                        z.count = 1;
                    }
                }
                6 => f[0].effect = Some(EffectFact::DestroyCreatures),
                _ => f[0].mode = "alternate".into(),
            }
            bot.observe(v);
            bot.observe_resources(Some(ResourceFacts {
                available: 4,
                capacity: 4,
            }));
            bot.observe_action_facts(&f);
            bot.observe_payments(&p);
            let (_, record) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
            assert_eq!(record["fallback"], true, "case {reason_case}");
            assert!(record["reason"].as_str().is_some());
        }
    }
    #[test]
    fn records_and_facts_do_not_survive_later_decisions() {
        let mut bot = BotResponder::new(AgentKind::Consequence.build());
        ready(&mut bot, "player-0", &facts());
        let _ = bot.respond(prompt("player-0", &["a", "b", "c"]));
        let (_, later) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_eq!(later["fallback"], true);
        ready(&mut bot, "player-0", &facts());
        bot.observe(view("player-0"));
        let (_, stale) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_eq!(stale["fallback"], true);
    }
    #[test]
    fn zero_toughness_has_no_board_gain() {
        let mut bot = BotResponder::new(AgentKind::Consequence.build());
        let mut f = facts();
        f[0].effect = Some(EffectFact::Creature {
            power: 4,
            toughness: 0,
        });
        ready(&mut bot, "player-0", &f);
        let (_, record) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_eq!(record["alternatives"][1]["after"]["own_board"], 0);
    }
    #[test]
    fn engine_created_observation_supports_real_choice_without_mutation() {
        use forge_foundation::{CardTypeLine, ColorSet, ManaCost, PhaseType, ZoneType};
        use manabrew_agent_interface::game_view_dto::GameViewDtoExt;
        use manabrew_engine::{
            card::Card,
            game::GameState,
            ids::{CardId, PlayerId},
            mana::ManaPool,
        };
        let player = PlayerId(0);
        let mut game = GameState::new(&["First", "Second"], 20);
        game.turn.phase = PhaseType::Main1;
        let mut ids = Vec::new();
        for (name, power) in [("Alpha", 2), ("Beta", 5)] {
            let card = Card::new(
                CardId(0),
                name.into(),
                player,
                CardTypeLine::parse("Creature"),
                ManaCost::zero(),
                ColorSet::COLORLESS,
                Some(power),
                Some(3),
                vec![],
                vec![],
            );
            let id = game.create_card(card);
            game.card_mut(id).zone = ZoneType::Hand;
            ids.push(id);
        }
        game.replace_zone_cards(ZoneType::Hand, player, ids.clone());
        let space = manabrew_engine::game_loop::GameLoop::new(2).action_space(&game, player, true);
        assert!(ids
            .iter()
            .all(|id| space.playable.iter().any(|play| play.card_id == *id)));
        let pools = vec![ManaPool::new(), ManaPool::new()];
        let before = GameViewDto::from_engine(&game, &pools, player, "fixture");
        let keys: Vec<String> = ids.iter().map(|id| format!("card-{}", id.0)).collect();
        let fs: Vec<ActionFacts> = keys
            .iter()
            .zip([2, 5])
            .map(|(id, power)| ActionFacts {
                action_id: id.clone(),
                mode: "normal".into(),
                mana_cost: Some("{0}".into()),
                target_scope: Some("self permanent".into()),
                effect: Some(EffectFact::Creature {
                    power,
                    toughness: 3,
                }),
                unknown: vec![],
                resolution_uncertain: true,
            })
            .collect();
        let ps: Vec<PaymentFacts> = keys
            .iter()
            .map(|id| PaymentFacts {
                action_id: id.clone(),
                spent: 0,
                remaining: 0,
            })
            .collect();
        let mut bot = BotResponder::new(AgentKind::Consequence.build());
        bot.observe(before.clone());
        bot.observe_resources(Some(ResourceFacts {
            available: 0,
            capacity: 0,
        }));
        bot.observe_action_facts(&fs);
        bot.observe_payments(&ps);
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let (_, record) = run(&mut bot, prompt("player-0", &refs));
        assert_eq!(record["fallback"], false);
        assert_eq!(record["chosen"], keys[1]);
        let after = GameViewDto::from_engine(&game, &pools, player, "fixture");
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
    }

    #[test]
    fn non_action_prompt_replaces_prior_record_and_legacy_facts_abstain() {
        let mut bot = BotResponder::new(AgentKind::Consequence.build());
        ready(&mut bot, "player-0", &facts());
        let _ = bot.respond(prompt("player-0", &["a", "b", "c"]));
        ready(&mut bot, "player-0", &facts());
        let boolean: AgentPrompt = serde_json::from_value(json!({"promptId":2,"decidingPlayerId":"player-0","input":{"type":"chooseBoolean","confirmLabel":"Yes","denyLabel":"No","presentation":{"title":"Confirm","targets":[]}}})).unwrap();
        let (_, record) = run(&mut bot, boolean);
        assert_eq!(record["fallback"], true);
        assert_eq!(record["reason"], "unsupported prompt");
        bot.observe(view("player-0"));
        bot.observe_resources(Some(ResourceFacts {
            available: 4,
            capacity: 4,
        }));
        bot.observe_action_facts(&facts());
        let (_, record) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_eq!(record["fallback"], true);
    }
    #[test]
    fn protected_wipe_and_creature_mana_are_not_scored_as_exact() {
        let mut v = view("player-0");
        let protected = CardDto {
            id: "enemy".into(),
            controller_id: "player-1".into(),
            owner_id: "player-1".into(),
            types: vec!["Creature".into()],
            power: Some("6".into()),
            toughness: Some("6".into()),
            keywords: vec!["Indestructible".into()],
            ..Default::default()
        };
        v.zones.push(ZoneDto {
            zone: ZoneKind::Battlefield,
            owner_id: "player-1".into(),
            cards: vec![CardView::Visible(protected)],
            count: 1,
        });
        for effect in [
            EffectFact::DestroyCreatures,
            EffectFact::ManaPermanent {
                produced: vec!["G".into()],
                creature: true,
            },
        ] {
            let mut f = facts();
            f[0].effect = Some(effect);
            let mut bot = BotResponder::new(AgentKind::Consequence.build());
            bot.observe(v.clone());
            bot.observe_resources(Some(ResourceFacts {
                available: 4,
                capacity: 4,
            }));
            bot.observe_action_facts(&f);
            bot.observe_payments(&payments());
            let (_, record) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
            assert_eq!(record["fallback"], true);
            assert_eq!(record["alternatives"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn default_control_ignores_fact_channels_on_populated_prompt() {
        let mut control = BotResponder::default();
        let mut supplied = BotResponder::new(AgentKind::Simple.build());
        control.observe(view("player-0"));
        supplied.observe(view("player-0"));
        supplied.observe_action_facts(&facts());
        supplied.observe_resources(Some(ResourceFacts {
            available: 4,
            capacity: 4,
        }));
        supplied.observe_payments(&payments());
        let p = prompt("player-0", &["a", "b", "c"]);
        assert_eq!(
            serde_json::to_value(control.respond(p.clone())).unwrap(),
            serde_json::to_value(supplied.respond(p)).unwrap()
        );
    }
    #[test]
    fn names_do_not_change_choice_but_public_threat_changes_values() {
        let mut original = view("player-0");
        let mut renamed = original.clone();
        for zone in &mut renamed.zones {
            for card in &mut zone.cards {
                if let CardView::Visible(card) = card {
                    card.identity.name = "A different name".into();
                }
            }
        }
        let select = |v: GameViewDto| {
            let mut bot = BotResponder::new(AgentKind::Strategic.build());
            bot.observe(v);
            bot.observe_resources(Some(ResourceFacts {
                available: 4,
                capacity: 4,
            }));
            bot.observe_action_facts(&facts());
            bot.observe_payments(&payments());
            run(&mut bot, prompt("player-0", &["a", "b", "c"])).1
        };
        let a = select(original.clone());
        let b = select(renamed);
        assert_eq!(a["chosen"], b["chosen"]);
        let enemy = CardDto {
            id: "e".into(),
            owner_id: "player-1".into(),
            controller_id: "player-1".into(),
            types: vec!["Creature".into()],
            power: Some("7".into()),
            toughness: Some("7".into()),
            ..Default::default()
        };
        original.zones.push(ZoneDto {
            zone: ZoneKind::Battlefield,
            owner_id: "player-1".into(),
            count: 1,
            cards: vec![CardView::Visible(enemy)],
        });
        let threatened = select(original);
        assert_ne!(
            a["alternatives"][1]["score"],
            threatened["alternatives"][1]["score"]
        );
    }
    #[test]
    fn attempted_action_is_retried_only_after_observation_changes() {
        let mut bot = BotResponder::new(AgentKind::Consequence.build());
        ready(&mut bot, "player-0", &facts());
        let (_, first) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        ready(&mut bot, "player-0", &facts());
        let (_, repeated) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_ne!(first["chosen"], repeated["chosen"]);
        let mut changed = view("player-0");
        changed.turn = 1;
        bot.observe(changed);
        bot.observe_resources(Some(ResourceFacts {
            available: 4,
            capacity: 4,
        }));
        bot.observe_action_facts(&facts());
        bot.observe_payments(&payments());
        let (_, next) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
        assert_eq!(first["chosen"], next["chosen"]);
    }
    #[test]
    fn empty_and_ambiguous_menu_ids_fall_back_for_both_policies() {
        for kind in [AgentKind::Strategic, AgentKind::Consequence] {
            let mut bot = BotResponder::new(kind.build());
            let mut empty_facts = facts();
            empty_facts[0].action_id = String::new();
            empty_facts[0].mana_cost = Some("{0}".into());
            let mut v = view("player-0");
            if let CardView::Visible(card) = &mut v.zones[0].cards[0] {
                card.id = "".into();
            }
            bot.observe(v);
            bot.observe_resources(Some(ResourceFacts {
                available: 0,
                capacity: 0,
            }));
            bot.observe_action_facts(&empty_facts);
            bot.observe_payments(&[PaymentFacts {
                action_id: "".into(),
                spent: 0,
                remaining: 0,
            }]);
            let (_, record) = run(&mut bot, prompt("player-0", &[""]));
            assert_eq!(record["fallback"], true);
            assert_eq!(record["reason"], "empty action identity");
            assert!(record["alternatives"].as_array().unwrap().is_empty());

            let duplicate: AgentPrompt = serde_json::from_value(json!({"promptId":2,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[
                {"type":"activateAbility","id":"a","cardId":"land","abilityIndex":0,"description":"mana","isManaAbility":true},
                {"type":"cast","id":"a","cardId":"a","mode":{"type":"normal"},"label":"cast"}
            ]}})).unwrap();
            ready(&mut bot, "player-0", &facts());
            let (_, record) = run(&mut bot, duplicate);
            assert_eq!(record["fallback"], true);
            assert_eq!(record["reason"], "duplicate action identity");
            assert!(record["alternatives"].as_array().unwrap().is_empty());

            // Populate the attempted set with an actual selection, then repeat its
            // identity twice. Validation must run before the attempted-action skip.
            ready(&mut bot, "player-0", &facts());
            let (_, first) = run(&mut bot, prompt("player-0", &["a", "b", "c"]));
            assert_eq!(first["fallback"], false);
            let chosen = first["chosen"].as_str().unwrap();
            assert_ne!(chosen, "pass");
            ready(&mut bot, "player-0", &facts());
            let (_, record) = run(&mut bot, prompt("player-0", &[chosen, chosen]));
            assert_eq!(record["fallback"], true);
            assert_eq!(record["reason"], "duplicate action identity");
            assert!(record["alternatives"].as_array().unwrap().is_empty());
        }
    }
}
