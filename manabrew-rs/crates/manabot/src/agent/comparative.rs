//! Experimental policies for the bounded native comparison. No hidden state or search.
use super::{ActionFacts, BotAgent, EffectFact, ResourceFacts, SimpleAi};
use manabrew_protocol::{
    game::{CardView, GameViewDto, StepKind, ZoneKind},
    prompts::{choose_action::ChooseActionOutput, PromptInput, PromptOutput},
    transport::AgentPrompt,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

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
    facts: Vec<ActionFacts>,
    resources: Option<ResourceFacts>,
    attempted: BTreeSet<String>,
    state: String,
    record: Option<Value>,
}
impl Comparative {
    pub fn new(variant: Variant) -> Self {
        Self {
            fallback: SimpleAi::default(),
            variant,
            view: None,
            facts: vec![],
            resources: None,
            attempted: BTreeSet::new(),
            state: String::new(),
            record: None,
        }
    }
}
impl BotAgent for Comparative {
    fn observe(&mut self, view: GameViewDto) {
        let state = serde_json::to_string(&view).unwrap_or_default();
        if state != self.state {
            self.attempted.clear();
            self.state = state;
        }
        self.fallback.observe(view.clone());
        self.view = Some(view);
    }
    fn observe_action_facts(&mut self, facts: &[ActionFacts]) {
        self.facts = facts.to_vec();
    }
    fn observe_resources(&mut self, facts: Option<ResourceFacts>) {
        self.resources = facts;
    }
    fn take_decision_record(&mut self) -> Option<Value> {
        self.record.take()
    }
    fn decide(&mut self, prompt: AgentPrompt) -> Option<PromptOutput> {
        let PromptInput::ChooseAction(input) = &prompt.input else {
            return self.fallback.decide(prompt);
        };
        let Some(view) = self.view.as_ref() else {
            return self.fallback.decide(prompt);
        };
        let me = &prompt.deciding_player_id;
        if view.active_player_id != *me
            || !matches!(view.step, StepKind::Main1 | StepKind::Main2)
            || !view.stack.is_empty()
        {
            self.record = Some(
                json!({"reason":"outside supported main-phase empty-stack subset", "fallback":true}),
            );
            return self.fallback.decide(prompt);
        }
        let mut own = 0i32;
        let mut enemy = 0i32;
        let mut own_vulnerable = 0i32;
        let mut enemy_vulnerable = 0i32;
        let Some(resources) = self.resources.as_ref() else {
            self.record = Some(json!({"reason":"unverified mana sources", "fallback":true}));
            return self.fallback.decide(prompt);
        };
        let mana = resources.available;
        let mut hand = 0usize;
        let mut library = None;
        for zone in &view.zones {
            if zone.zone == ZoneKind::Hand && zone.owner_id == *me {
                hand = zone.count;
            }
            if zone.zone == ZoneKind::Library && zone.owner_id == *me {
                library = Some(zone.count);
            }
            if zone.zone != ZoneKind::Battlefield {
                continue;
            }
            for c in &zone.cards {
                let CardView::Visible(c) = c else { continue };
                if c.phased_out {
                    continue;
                }
                if !c.types.iter().any(|t| t == "Creature") {
                    continue;
                }
                let Some(power) = c.power.as_deref().and_then(|p| p.parse::<i32>().ok()) else {
                    self.record =
                        Some(json!({"reason":"unknown battlefield power", "fallback":true}));
                    return self.fallback.decide(prompt);
                };
                let protected = c
                    .keywords
                    .iter()
                    .chain(c.granted_keywords.iter())
                    .any(|k| k.eq_ignore_ascii_case("indestructible"));
                if c.controller_id == *me {
                    own += power;
                    if !protected {
                        own_vulnerable += power;
                    }
                } else {
                    enemy += power;
                    if !protected {
                        enemy_vulnerable += power;
                    }
                }
            }
        }
        let before = ProjectedPosition {
            mana,
            capacity: resources.capacity,
            hand: hand as i32,
            own_board: own,
            enemy_board: enemy,
        };
        let (before_value, before_components) = before.value();
        let pass_score = if matches!(self.variant, Variant::Consequence) {
            before_value
        } else {
            0
        };
        let mut alternatives = vec![
            json!({"action":"pass","cost":0,"score":pass_score,"before":before,"after":before,"components":before_components,"reason":"retain resources"}),
        ];
        let mut ranked: Vec<(i32, String)> = vec![(pass_score, String::new())];
        let facts = self.facts.clone();
        for action in &input.actions {
            if matches!(
                &action.kind,
                manabrew_protocol::prompts::common::AvailableActionKind::UndoMana { .. }
                    | manabrew_protocol::prompts::common::AvailableActionKind::ActivateAbility(
                        manabrew_protocol::prompts::common::ActivatableAbilityInfo {
                            is_mana_ability: true,
                            ..
                        }
                    )
            ) {
                continue;
            }
            if self.attempted.contains(&action.id) {
                continue;
            }
            let Some(f) = facts.iter().find(|f| f.action_id == action.id) else {
                self.record = Some(
                    json!({"reason":"missing action facts", "fallback":true,"alternatives":alternatives}),
                );
                return self.fallback.decide(prompt);
            };
            let (Some(effect), Some(_cost)) = (&f.effect, &f.mana_cost) else {
                self.record = Some(
                    json!({"reason":"unsupported offered action or cost", "fallback":true,"facts":facts}),
                );
                return self.fallback.decide(prompt);
            };
            let card_id = match &action.kind {
                manabrew_protocol::prompts::common::AvailableActionKind::Cast {
                    card_id, ..
                } => card_id,
                _ => {
                    self.record = Some(json!({"reason":"unsupported action kind","fallback":true}));
                    return self.fallback.decide(prompt);
                }
            };
            let Some(card) = view
                .zones
                .iter()
                .flat_map(|z| &z.cards)
                .find_map(|c| match c {
                    CardView::Visible(c) if &c.id == card_id => Some(c),
                    _ => None,
                })
            else {
                return self.fallback.decide(prompt);
            };
            let cost = card.cmc.max(0);
            let left = (mana - cost).max(0);
            let threat = (enemy - own).max(0);
            if matches!(effect, EffectFact::Draw { .. }) && library.is_none() {
                self.record = Some(
                    json!({"reason":"library count unavailable", "fallback":true, "facts":facts}),
                );
                return self.fallback.decide(prompt);
            }
            let unsafe_draw = matches!(effect, EffectFact::Draw { cards } if *cards > library.unwrap_or(0) as i32);
            let dies_on_entry =
                matches!(effect, EffectFact::Creature { toughness, .. } if *toughness <= 0);
            let mut after = before.clone();
            after.mana = left;
            after.hand = (after.hand - 1).max(0);
            match effect {
                EffectFact::Creature { power, toughness } if *toughness > 0 => {
                    after.own_board += power
                }
                EffectFact::Draw { cards } if !unsafe_draw => after.hand += cards,
                EffectFact::DestroyCreatures => {
                    after.own_board -= own_vulnerable;
                    after.enemy_board -= enemy_vulnerable;
                }
                EffectFact::ManaPermanent { .. } => after.capacity += 1,
                _ => (),
            }
            let (projected, components) = after.value();
            let (score, why) = if unsafe_draw {
                (-10000, "draw can exhaust the library")
            } else if dies_on_entry {
                (-10000, "zero toughness does not persist")
            } else {
                match self.variant {
                    Variant::Consequence => (
                        projected,
                        "one shared value function applied to projected position",
                    ),
                    Variant::Strategic => match effect {
                        EffectFact::Creature { power, .. } => (
                            2 + power.min(&5) + if threat > 0 { 3 } else { 0 },
                            "develop a blocker and attacker",
                        ),
                        EffectFact::Draw { cards } => (
                            cards * 2 + if hand <= 3 { 4 } else { -2 }
                                - if threat > 5 { 3 } else { 0 },
                            "replenish access",
                        ),
                        EffectFact::DestroyCreatures => (
                            enemy_vulnerable - own_vulnerable - 2,
                            "compare vulnerable creatures",
                        ),
                        EffectFact::ManaPermanent { creature, .. } => (
                            (if threat > 5 {
                                1
                            } else if mana < 5 {
                                6
                            } else {
                                2
                            }) - if *creature { 1 } else { 0 },
                            "invest in future mana",
                        ),
                    },
                }
            };
            alternatives.push(json!({"action":action.id,"cost":f.mana_cost,"effect":effect,"score":score,"before":before,"after":after,"components":components,"uncertainty":"conditional on resolution; wipe outcomes range from unchanged board to projected vulnerable removals; replacements and prevention are not forecast","reason":why}));
            ranked.push((score, action.id.clone()));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let chosen = ranked[0].1.clone();
        self.record = Some(
            json!({"fallback":false,"alternatives":alternatives,"chosen":if chosen.is_empty(){"pass"}else{&chosen},"public_context":{"own_power":own,"enemy_power":enemy,"own_vulnerable":own_vulnerable,"enemy_vulnerable":enemy_vulnerable,"hand":hand,"verified_available_mana":mana,"mana_capacity":resources.capacity,"library_count":library},"assumption":"scores model potential effects, not guaranteed outcomes"}),
        );
        if chosen.is_empty() {
            Some(PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                until: None,
                exhaust_stack: false,
            }))
        } else {
            self.attempted.insert(chosen.clone());
            Some(PromptOutput::ChooseAction(ChooseActionOutput::Act {
                action_id: chosen,
            }))
        }
    }
}
