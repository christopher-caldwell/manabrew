//! Context-dependent development/conversion choices. Uses only the public view.
use super::*;

impl SimpleAi {
    fn ready_creature(card: &CardDto) -> bool {
        card.types.iter().any(|t| t == "Creature")
            && !card.tapped
            && !card.phased_out
            && (!card.summoning_sick || Self::has_keyword(card, "Haste"))
            && !Self::has_keyword(card, "Defender")
    }

    pub(super) fn contextual_activation_score(
        &self,
        info: &ActivatableAbilityInfo,
        player: &str,
    ) -> Option<i32> {
        let text = info.description.to_ascii_lowercase();
        let team = text.contains("each creature you control")
            || text.contains("creatures you control get");
        let pump = text.contains("+1/+1") || text.contains("+2/+2");
        let conversion = team && pump;
        let removal = text.starts_with("destroy target creature")
            || text.starts_with("exile target creature");
        if !conversion && !removal {
            return None;
        }
        let view = self.view.as_ref()?;
        let life = view
            .players
            .iter()
            .find(|p| p.id == player)
            .map_or(40, |p| p.life);
        let creatures: Vec<_> = self
            .battlefield(player)
            .filter(|c| c.types.iter().any(|t| t == "Creature") && !c.phased_out)
            .collect();
        let ready: Vec<_> = creatures
            .iter()
            .copied()
            .filter(|c| Self::ready_creature(c))
            .collect();
        let aerial_threat = self
            .opponent_creatures(player)
            .into_iter()
            .filter(|c| Self::has_keyword(c, "Flying") && Self::stat(c.power.as_deref()) >= life)
            .count();
        let aerial_defenders = creatures
            .iter()
            .filter(|c| {
                !c.tapped && (Self::has_keyword(c, "Flying") || Self::has_keyword(c, "Reach"))
            })
            .count();
        let cost = info.cost.as_deref().unwrap_or("");
        let mana = cost
            .split('{')
            .skip(1)
            .filter_map(|p| p.split('}').next()?.parse::<i32>().ok())
            .sum::<i32>();
        let loyalty = if cost.contains("SubCounter") && cost.contains("Loyalty") {
            cost.split("Literal(")
                .nth(1)
                .and_then(|s| s.split(')').next())
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0)
        } else {
            0
        };
        if removal {
            return Some(if aerial_threat > aerial_defenders {
                420 - loyalty * 15 - mana * 25
            } else {
                200 - mana * 25
            });
        }
        if creatures.is_empty() {
            return Some(-1);
        }
        let flying = text.contains("flying");
        let lasting = text.contains("counter") || text.contains("until your next turn");
        let can_attack = view.active_player_id == player && view.step == StepKind::Main1;
        let upgraded = ready
            .iter()
            .filter(|c| flying && !Self::has_keyword(c, "Flying"))
            .count() as i32;
        // Do not spend resources converting an already sufficient clear attack.
        // This deliberately requires a visible closing opportunity, not raw power alone.
        let opponents: Vec<_> = view
            .players
            .iter()
            .filter(|p| p.id != player && p.life > 0)
            .collect();
        let closes = opponents.len() == 1
            && ready
                .iter()
                .filter(|c| self.should_attack(&c.id, &opponents[0].id))
                .filter(|c| {
                    self.battlefield(&opponents[0].id).all(|b| {
                        !b.types.iter().any(|t| t == "Creature")
                            || b.tapped
                            || (Self::has_keyword(c, "Flying")
                                && !Self::has_keyword(b, "Flying")
                                && !Self::has_keyword(b, "Reach"))
                    })
                })
                .map(|c| Self::stat(c.power.as_deref()))
                .sum::<i32>()
                >= opponents[0].life;
        let mut value = if can_attack && !closes {
            ready.len() as i32 * 15 + upgraded * 15
        } else {
            0
        };
        if lasting {
            value += creatures.len() as i32 * 4;
        }
        if flying && text.contains("until your next turn") && aerial_threat > aerial_defenders {
            value += creatures
                .iter()
                .filter(|c| !c.tapped && !Self::has_keyword(c, "Flying"))
                .count()
                .min(aerial_threat) as i32
                * 250;
        }
        let source_dies = self.card(&info.card_id).is_some_and(|c| {
            c.counters.get("Loyalty").copied().unwrap_or(99) as i32 <= loyalty && loyalty > 0
        });
        Some(60 + value - mana * 40 - loyalty * 35 - if source_dies { 180 } else { 0 })
    }

    pub(super) fn pointless_x_cast(&self, card: &CardDto, player: &str) -> bool {
        if !card.mana_cost.contains('X') {
            return false;
        }
        let x = card.maximum_cast_x.map(|x| x as i32).unwrap_or_else(|| {
            let count = card.mana_cost.matches('X').count().max(1) as i32;
            ((self.mana_sources(player).2 - card.cmc - card.commander_tax.unwrap_or(0)) / count)
                .max(0)
        });
        if x > 0 {
            return false;
        }
        let text = card.text.to_ascii_lowercase();
        let creature = card.types.iter().any(|t| t == "Creature");
        let survives = creature && Self::stat(card.toughness.as_deref()) > 0;
        if survives || self.zero_x_payoff(card, player) {
            return false;
        }
        // A zero-sized X creature and an X-only damage/draw/token effect have
        // no useful result without a real cast, entry, death or static payoff.
        (creature && Self::stat(card.toughness.as_deref()) == 0)
            || text.contains("x damage")
            || text.contains("draw x")
            || text.contains("create x")
    }

    fn zero_x_payoff(&self, card: &CardDto, player: &str) -> bool {
        let creature = card.types.iter().any(|t| t == "Creature");
        let artifact = card.types.iter().any(|t| t == "Artifact");
        self.battlefield(player).any(|source| {
            let text = source.text.to_ascii_lowercase();
            let matches_type = |clause: &str| {
                if clause.contains("angel") {
                    card.subtypes.iter().any(|t| t == "Angel")
                } else if clause.contains("artifact") {
                    artifact
                } else if clause.contains("noncreature") {
                    !creature
                } else if clause.contains("creature") {
                    creature
                } else {
                    true
                }
            };
            let reward = text.contains("draw")
                || text.contains("create")
                || text.contains("gain")
                || text.contains("damage")
                || text.contains("counter");
            let triggered = text.split(['.', '\n']).any(|clause| {
                reward
                    && matches_type(clause)
                    && (clause.contains("whenever you cast")
                        || (creature
                            && (clause.contains("creature enters")
                                || clause.contains("creature you control dies"))))
            });
            let boost = creature
                && text
                    .split("creatures you control get +")
                    .nth(1)
                    .and_then(|tail| tail.split('/').nth(1))
                    .and_then(|tail| {
                        tail.trim_start_matches('+')
                            .split(|c: char| !c.is_ascii_digit())
                            .next()?
                            .parse::<i32>()
                            .ok()
                    })
                    .is_some_and(|toughness| toughness > 0)
                && matches_type(&text);
            triggered || boost
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manabrew_protocol::game::{PlayerDto, ZoneDto};
    use serde_json::{json, Value};
    fn decide(bot: &mut SimpleAi, prompt: Value) -> Value {
        serde_json::to_value(bot.decide(serde_json::from_value(prompt).unwrap()).unwrap()).unwrap()
            ["output"]
            .clone()
    }
    fn archived(raw: &str) -> (SimpleAi, Value) {
        let d: Value = serde_json::from_str(raw).unwrap();
        let mut bot = SimpleAi::default();
        bot.observe(serde_json::from_value(d["view"].clone()).unwrap());
        (bot, d["prompt"].clone())
    }
    #[test]
    fn archived_ready_cats_choose_conversion_and_opening_x_creature_is_retained() {
        let (mut bot, prompt) = archived(include_str!("wide_army_regression.json"));
        let out = decide(&mut bot, prompt);
        assert_eq!(out["actionId"], "ability:card-17:1", "{out}");
        let (mut bot, prompt) = archived(include_str!("zero_x_regression.json"));
        let out = decide(&mut bot, prompt);
        assert_eq!(out["type"], "pass", "{out}");
    }
    fn creature(id: usize, power: i32, flying: bool, sick: bool) -> CardDto {
        CardDto {
            id: format!("card-{id}"),
            controller_id: "player-0".into(),
            owner_id: "player-0".into(),
            types: vec!["Creature".into()],
            power: Some(power.to_string()),
            toughness: Some("1".into()),
            summoning_sick: sick,
            keywords: if flying {
                vec!["Flying".into()]
            } else {
                vec![]
            },
            ..Default::default()
        }
    }
    fn player(id: &str, life: i32) -> PlayerDto {
        let d: Value = serde_json::from_str(include_str!("wide_army_regression.json")).unwrap();
        let mut p: PlayerDto = serde_json::from_value(d["view"]["players"][0].clone()).unwrap();
        p.id = id.into();
        p.life = life;
        p
    }
    fn activation_table(
        count: usize,
        sick: bool,
        flying: bool,
        life: i32,
        enemy_life: i32,
        threat: bool,
        cost: &str,
    ) -> (SimpleAi, Value) {
        let source = CardDto {
            id: "card-1".into(),
            controller_id: "player-0".into(),
            types: vec!["Planeswalker".into()],
            counters: std::collections::BTreeMap::from([("Loyalty".into(), 5)]),
            ..Default::default()
        };
        let mut cards = vec![CardView::Visible(source)];
        cards.extend((0..count).map(|i| CardView::Visible(creature(i + 10, 1, flying, sick))));
        let mut enemy = creature(90, 4, true, false);
        enemy.controller_id = "player-1".into();
        enemy.owner_id = "player-1".into();
        enemy.tapped = true;
        let view = GameViewDto {
            active_player_id: "player-0".into(),
            step: StepKind::Main1,
            players: vec![player("player-0", life), player("player-1", enemy_life)],
            zones: vec![
                ZoneDto {
                    zone: ZoneKind::Battlefield,
                    owner_id: "player-0".into(),
                    count: cards.len(),
                    cards,
                },
                ZoneDto {
                    zone: ZoneKind::Battlefield,
                    owner_id: "player-1".into(),
                    count: usize::from(threat),
                    cards: if threat {
                        vec![CardView::Visible(enemy)]
                    } else {
                        vec![]
                    },
                },
            ],
            ..Default::default()
        };
        let mut bot = SimpleAi::default();
        bot.observe(view);
        let prompt = json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[
            {"type":"activateAbility","id":"produce","cardId":"card-1","abilityIndex":0,"isManaAbility":false,"cost":"AddCounter { amount: Literal(1), counter_type: Loyalty }","description":"Create a 1/1 Soldier creature token."},
            {"type":"activateAbility","id":"convert","cardId":"card-1","abilityIndex":1,"isManaAbility":false,"cost":cost,"description":"Put a +1/+1 counter on each creature you control. Those creatures gain flying until your next turn."}
        ]}});
        (bot, prompt)
    }
    #[test]
    fn production_conversion_and_restraint_follow_opportunities_and_costs() {
        for (count, sick, flying, life, enemy_life, threat, cost, expected) in [
            (12, false, false, 1, 27, true, "{0}", "convert"),
            (2, false, true, 40, 27, false, "{0}", "produce"),
            (12, true, false, 40, 27, false, "{0}", "produce"),
            (12, false, true, 40, 10, false, "{0}", "produce"),
            (2, true, false, 1, 27, true, "{0}", "convert"),
            (12, false, false, 40, 27, false, "{12}", "produce"),
            (
                2,
                false,
                false,
                40,
                27,
                false,
                "SubCounter { amount: Literal(5), counter_type: Loyalty }",
                "produce",
            ),
        ] {
            let (mut bot, prompt) =
                activation_table(count, sick, flying, life, enemy_life, threat, cost);
            let out = decide(&mut bot, prompt);
            assert_eq!(
                out["actionId"], expected,
                "count={count}, sick={sick}, flying={flying}, cost={cost}: {out}"
            );
        }
        let (mut bot, mut prompt) = activation_table(0, false, false, 40, 27, false, "{0}");
        prompt["input"]["actions"].as_array_mut().unwrap().remove(0);
        assert_eq!(
            decide(&mut bot, prompt)["type"],
            "pass",
            "no conversion opportunity"
        );
    }
    #[test]
    fn urgent_single_threat_can_favor_removal_over_costly_conversion() {
        let (mut bot, mut prompt) = activation_table(1, false, false, 1, 27, true, "{4}");
        prompt["input"]["actions"].as_array_mut().unwrap().push(json!({"type":"activateAbility","id":"remove","cardId":"card-1","abilityIndex":2,"isManaAbility":false,"cost":"{0}","description":"Destroy target creature an opponent controls."}));
        assert_eq!(decide(&mut bot, prompt)["actionId"], "remove");
    }
    #[test]
    fn zero_x_is_contextual_and_positive_x_is_deployed() {
        for (max_x, payoff, expected) in [
            (0, "", "pass"),
            (1, "", "act"),
            (3, "", "act"),
            (
                0,
                "Whenever you cast a creature spell, create a 1/1 Soldier creature token.",
                "act",
            ),
            (0, "Creatures you control get +1/+1.", "act"),
            (0, "Creatures you control get +1/+0.", "pass"),
            (
                0,
                "Whenever a creature you control dies, draw a card.",
                "act",
            ),
            (
                0,
                "Whenever a creature enters the battlefield under your control, gain 1 life.",
                "act",
            ),
            (
                0,
                "Whenever an Angel you control dies, draw a card.",
                "pass",
            ),
        ] {
            let (mut bot, prompt) = archived(include_str!("zero_x_regression.json"));
            for zone in &mut bot.view.as_mut().unwrap().zones {
                for card in &mut zone.cards {
                    if let CardView::Visible(c) = card {
                        if c.id == "card-170" {
                            c.maximum_cast_x = Some(max_x);
                        }
                    }
                }
            }
            if !payoff.is_empty() {
                let zone = bot
                    .view
                    .as_mut()
                    .unwrap()
                    .zones
                    .iter_mut()
                    .find(|z| z.zone == ZoneKind::Battlefield && z.owner_id == "player-1")
                    .unwrap();
                zone.cards.push(CardView::Visible(CardDto {
                    id: "card-999".into(),
                    controller_id: "player-1".into(),
                    owner_id: "player-1".into(),
                    text: payoff.into(),
                    ..Default::default()
                }));
            }
            // Rebuild view's card lookup after adding the public payoff.
            let view = bot.view.take().unwrap();
            bot.observe(view);
            let out = decide(&mut bot, prompt);
            assert_eq!(
                out["type"], expected,
                "max_x={max_x}, payoff={payoff}: {out}"
            );
        }
    }
}

#[cfg(test)]
mod engine_fact_tests {
    use super::*;
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
    use manabrew_agent_interface::game_view_dto::GameViewDtoExt;
    use manabrew_engine::{
        card::Card,
        game::GameState,
        ids::{CardId, PlayerId},
        mana::ManaPool,
    };
    use serde_json::json;
    #[test]
    fn live_engine_x_fact_reaches_the_actual_cast_decision_with_restricted_sources() {
        for (mana_count, restricted, expected_x, expected_choice) in [
            (1, false, 0, "pass"),
            (2, false, 1, "act"),
            (6, false, 3, "act"),
            (2, true, 0, "pass"),
        ] {
            let p = PlayerId(0);
            let mut g = GameState::new(&["A", "B"], 40);
            g.turn.phase = forge_foundation::PhaseType::Main1;
            let mut card = Card::new(
                CardId(0),
                "Counter Construct".into(),
                p,
                CardTypeLine::parse("Artifact Creature Construct"),
                ManaCost::parse("X X"),
                ColorSet::COLORLESS,
                Some(0),
                Some(0),
                vec!["etbCounter:P1P1:X".into()],
                vec![],
            );
            card.set_s_var("X", "Count$xPaid");
            let id = g.create_card(card);
            g.move_card(id, ZoneType::Hand, p);
            for _ in 0..mana_count {
                let restriction = if restricted {
                    " | RestrictValid$ Spell.Angel"
                } else {
                    ""
                };
                let card = Card::new(
                    CardId(0),
                    "Source".into(),
                    p,
                    CardTypeLine::parse("Artifact"),
                    ManaCost::parse("0"),
                    ColorSet::COLORLESS,
                    None,
                    None,
                    vec![],
                    vec![format!("AB$ Mana | Cost$ T | Produced$ C{restriction}")],
                );
                let mana = g.create_card(card);
                g.move_card(mana, ZoneType::Battlefield, p);
            }
            let pools = vec![ManaPool::new(), ManaPool::new()];
            let view = GameViewDto::from_engine(&g, &pools, p, "regression");
            let dto = view
                .zones
                .iter()
                .flat_map(|z| &z.cards)
                .find_map(|c| match c {
                    CardView::Visible(c) if c.id == "card-0" => Some(c),
                    _ => None,
                })
                .unwrap();
            assert_eq!(dto.maximum_cast_x, Some(expected_x));
            let mut bot = SimpleAi::default();
            bot.observe(view);
            let prompt=serde_json::from_value(json!({"promptId":1,"decidingPlayerId":"player-0","input":{"type":"chooseAction","actions":[{"type":"cast","id":"cast","cardId":"card-0","mode":{"type":"normal"},"label":"Cast normally"}]}})).unwrap();
            let out = serde_json::to_value(bot.decide(prompt).unwrap()).unwrap();
            assert_eq!(
                out["output"]["type"], expected_choice,
                "{mana_count}, restricted={restricted}: {out}"
            );
        }
    }

    #[test]
    fn an_x_deployment_preserves_a_held_answer_when_the_remainder_can_pay_it() {
        let d: serde_json::Value =
            serde_json::from_str(include_str!("zero_x_regression.json")).unwrap();
        for (lands, holds) in [(2, true), (3, false)] {
            let mut view: GameViewDto = serde_json::from_value(d["view"].clone()).unwrap();
            let zone = view
                .zones
                .iter_mut()
                .find(|z| z.zone == ZoneKind::Battlefield && z.owner_id == "player-1")
                .unwrap();
            let land = zone
                .cards
                .iter()
                .find_map(|c| match c {
                    CardView::Visible(c) => Some(c.clone()),
                    _ => None,
                })
                .unwrap();
            for i in 1..lands {
                let mut clone = land.clone();
                clone.id = format!("card-extra-{i}");
                zone.cards.push(CardView::Visible(clone));
            }
            for zone in &mut view.zones {
                for card in &mut zone.cards {
                    if let CardView::Visible(c) = card {
                        if c.id == "card-170" {
                            c.maximum_cast_x = Some(1);
                        }
                    }
                }
            }
            let mut bot = SimpleAi::default();
            bot.observe(view);
            bot.reserve = Some("{1}".into());
            let action:AvailableAction=serde_json::from_value(json!({"type":"cast","id":"cast","cardId":"card-170","mode":{"type":"normal"},"label":"Cast normally"})).unwrap();
            assert_eq!(
                bot.dips_into_reserve(&action, "player-1"),
                holds,
                "lands={lands}"
            );
        }
    }
}
