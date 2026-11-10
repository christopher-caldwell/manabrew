//! Public-board combat estimates. Hidden responses and arbitrary scripted
//! effects remain uncertain; this is a bounded allocation search, not a game solver.
use super::*;
use manabrew_protocol::prompts::{
    choose_attackers::AttackerOptionDto, choose_blockers::BlockableAttackerDto,
};

impl SimpleAi {
    pub(super) fn defense_reserve(&self, me: &str, attackers: &[String]) -> HashSet<String> {
        let mut keep = HashSet::new();
        let Some(view) = &self.view else {
            return keep;
        };
        let life = self.life_of(me);
        for opponent in view.players.iter().filter(|p| p.id != me && p.life > 0) {
            let mut threats: Vec<_> = self
                .battlefield(&opponent.id)
                .filter(|c| {
                    c.types.iter().any(|t| t == "Creature")
                        && !c.phased_out
                        && !Self::has_keyword(c, "Defender")
                        && Self::stat(c.power.as_deref()) > 0
                })
                .collect();
            threats.sort_by_key(|c| std::cmp::Reverse(Self::stat(c.power.as_deref())));
            let mut incoming: i32 = threats
                .iter()
                .map(|c| {
                    Self::stat(c.power.as_deref())
                        * if Self::has_keyword(c, "Double strike") {
                            2
                        } else {
                            1
                        }
                })
                .sum();
            if !Self::life_in_danger(life, incoming, false) {
                continue;
            }
            let mut defenders: Vec<_> = self
                .battlefield(me)
                .filter(|c| {
                    c.types.iter().any(|t| t == "Creature")
                        && !c.tapped
                        && !c.phased_out
                        && !c.text.to_ascii_lowercase().contains("can't block")
                })
                .collect();
            // Summoning sickness does not stop a creature from blocking. Bodies
            // already staying home and vigilant attackers cost no attack damage.
            defenders.sort_by_key(|c| {
                (
                    attackers.contains(&c.id) && !Self::has_keyword(c, "Vigilance"),
                    self.importance(c),
                )
            });
            let mut used = HashSet::new();
            for threat in threats {
                if !Self::life_in_danger(life, incoming, false) {
                    break;
                }
                let Some(blocker) = defenders
                    .iter()
                    .find(|c| !used.contains(&c.id) && self.visible_can_block(c, threat))
                else {
                    continue;
                };
                used.insert(blocker.id.clone());
                let power = Self::stat(threat.power.as_deref())
                    * if Self::has_keyword(threat, "Double strike") {
                        2
                    } else {
                        1
                    };
                let stopped = if Self::has_keyword(threat, "Trample") {
                    Self::stat(blocker.toughness.as_deref()).min(power)
                } else {
                    power
                };
                incoming -= stopped;
                if attackers.contains(&blocker.id) && !Self::has_keyword(blocker, "Vigilance") {
                    keep.insert(blocker.id.clone());
                }
            }
        }
        keep
    }

    pub(super) fn visible_can_block(&self, blocker: &CardDto, attacker: &CardDto) -> bool {
        if !blocker.types.iter().any(|ty| ty == "Creature")
            || blocker.tapped
            || blocker.phased_out
            || blocker.text.to_ascii_lowercase().contains("can't block")
        {
            return false;
        }
        let text = attacker.text.to_ascii_lowercase();
        if Self::has_keyword(attacker, "Unblockable") || text.contains("can't be blocked") {
            return false;
        }
        if Self::has_keyword(attacker, "Flying")
            && !Self::has_keyword(blocker, "Flying")
            && !Self::has_keyword(blocker, "Reach")
        {
            return false;
        }
        if Self::has_keyword(attacker, "Shadow") != Self::has_keyword(blocker, "Shadow") {
            return false;
        }
        if Self::has_keyword(attacker, "Horsemanship")
            && !Self::has_keyword(blocker, "Horsemanship")
        {
            return false;
        }
        if Self::has_keyword(attacker, "Skulk")
            && Self::stat(blocker.power.as_deref()) > Self::stat(attacker.power.as_deref())
        {
            return false;
        }
        true
    }

    fn attack_reward(&self, card: &CardDto, player: &str) -> i32 {
        let mut reward = 0;
        // Trigger value is conditional on an actual resource opportunity. In
        // particular, attack mana has no spell value with an empty hand.
        let usable_mana = |amount| {
            self.zone_cards(player, ZoneKind::Hand).iter().any(|spell| {
                (spell.types.iter().any(|ty| ty == "Instant") || Self::has_keyword(spell, "Flash"))
                    && !Self::roles(spell).contains(Roles::COUNTERSPELL)
                    && spell.cmc <= self.available_mana(player) + amount
            })
        };
        for keyword in &card.keywords {
            if let Some(amount) = keyword
                .strip_prefix("Firebending:")
                .and_then(|n| n.parse::<i32>().ok())
            {
                if usable_mana(amount) {
                    reward += 12;
                }
            }
        }
        for source in self.battlefield(player) {
            for sentence in source.text.to_ascii_lowercase().split('.') {
                let self_attack = source.id == card.id
                    && !sentence.contains("another")
                    && (sentence.contains("this creature attacks")
                        || (!source.identity.name.is_empty()
                            && sentence.contains(&format!(
                                "{} attacks",
                                source.identity.name.to_ascii_lowercase()
                            ))));
                let each_attack = sentence.contains("a creature you control attacks");
                if !sentence.contains("whenever") || !(self_attack || each_attack) {
                    continue;
                }
                reward += self.attack_trigger_reward(sentence, player, usable_mana(3));
            }
        }
        // Myriad's free copies can matter even when the original is blocked.
        // Do not award them in a duel or for eliminated opponents.
        if Self::has_keyword(card, "Myriad")
            && self.view.as_ref().is_some_and(|view| {
                view.players
                    .iter()
                    .filter(|p| p.id != player && p.life > 0)
                    .count()
                    > 1
            })
        {
            reward += 18;
        }
        reward
    }

    fn attack_trigger_reward(&self, sentence: &str, player: &str, usable_mana: bool) -> i32 {
        let mut reward = 0;
        if sentence.contains("draw") && self.library_count(player) > 1 {
            reward += 14;
        }
        if sentence.contains("create") {
            reward += 14;
        }
        if sentence.contains("add ") && usable_mana {
            reward += 12;
        }
        reward
    }

    fn declaration_reward(&self, assignments: &[AttackAssignment], player: &str) -> i32 {
        if assignments.is_empty() {
            return 0;
        }
        self.battlefield(player)
            .map(|source| {
                source
                    .text
                    .to_ascii_lowercase()
                    .split('.')
                    .filter(|sentence| {
                        sentence.contains("whenever")
                            && (sentence.contains("you attack")
                                || sentence.contains("one or more creatures you control attack"))
                            && (!sentence.contains("two or more") || assignments.len() >= 2)
                    })
                    .map(|sentence| self.attack_trigger_reward(sentence, player, false))
                    .sum::<i32>()
            })
            .sum()
    }

    /// Forecast simultaneous first-strike/normal damage, with each blocker
    /// available once. Returns damage to the defender and material exchange.
    fn forecast_exchange(&self, attacker: &CardDto, blockers: &[&CardDto]) -> (i32, i32, i32) {
        let Some(a) = self.combatant(&attacker.id) else {
            return (0, 0, 0);
        };
        let mut bs: Vec<_> = blockers
            .iter()
            .filter_map(|c| self.combatant(&c.id))
            .collect();
        bs.sort_by_key(|b| std::cmp::Reverse(b.value));
        let mut alive = vec![true; bs.len()];
        let mut wounds = vec![0; bs.len()];
        let mut a_alive = true;
        let mut a_wounds = 0;
        let mut damage = 0;
        let mut dealt = 0;
        for first in [true, false] {
            let a_strikes = a_alive
                && if first {
                    a.first_strike
                } else {
                    !a.first_strike || a.double_strike
                };
            let back: Vec<_> = bs
                .iter()
                .enumerate()
                .filter(|(i, b)| {
                    alive[*i]
                        && if first {
                            b.first_strike
                        } else {
                            !b.first_strike || b.double_strike
                        }
                })
                .map(|(i, _)| i)
                .collect();
            let mut kill_a = false;
            if a_strikes {
                let mut remaining = a.power;
                let targets: Vec<_> = (0..bs.len()).filter(|&i| alive[i]).collect();
                for (n, &i) in targets.iter().enumerate() {
                    let lethal = if a.deathtouch {
                        1
                    } else {
                        (bs[i].lethal - wounds[i]).max(1)
                    };
                    let hit = if !a.trample && n + 1 == targets.len() {
                        remaining
                    } else {
                        remaining.min(lethal)
                    };
                    wounds[i] += hit;
                    remaining -= hit;
                    dealt += hit;
                    if hit > 0
                        && !bs[i].indestructible
                        && (a.deathtouch || wounds[i] >= bs[i].lethal)
                    {
                        alive[i] = false;
                    }
                }
                if bs.is_empty() || a.trample {
                    damage += remaining;
                    dealt += remaining;
                }
            }
            // All creatures that began this damage step alive deal together.
            for i in back {
                if !a_alive {
                    break;
                }
                a_wounds += bs[i].power;
                kill_a |= bs[i].power > 0 && bs[i].deathtouch;
            }
            if !a.indestructible && (kill_a || a_wounds >= a.lethal) {
                a_alive = false;
            }
        }
        let killed_value: i32 = blockers
            .iter()
            .filter(|c| {
                bs.iter()
                    .enumerate()
                    .any(|(i, b)| b.id == c.id && !alive[i])
            })
            .map(|c| self.importance(c))
            .sum();
        let loss = if a_alive {
            0
        } else {
            self.importance(attacker)
        };
        let gain = if Self::has_keyword(attacker, "Lifelink") {
            dealt
        } else {
            0
        };
        (damage, killed_value - loss, gain)
    }

    pub(super) fn attack_plan_score(
        &self,
        assignments: &[AttackAssignment],
        me: &str,
        keep: &HashSet<String>,
    ) -> i32 {
        let rewards = assignments
            .iter()
            .filter_map(|a| {
                self.card(&a.attacker_id)
                    .map(|c| (c.id.clone(), self.attack_reward(c, me)))
            })
            .collect();
        self.score_attack_plan(assignments, me, keep, &rewards)
    }

    fn score_attack_plan(
        &self,
        assignments: &[AttackAssignment],
        me: &str,
        keep: &HashSet<String>,
        rewards: &HashMap<String, i32>,
    ) -> i32 {
        let Some(view) = &self.view else {
            return assignments.len() as i32;
        };
        let mut score = self.declaration_reward(assignments, me);
        let mut damage: HashMap<String, i32> = HashMap::new();
        let mut defenders: Vec<_> = assignments
            .iter()
            .map(|a| {
                self.card(&a.target_id)
                    .map_or(a.target_id.as_str(), |c| c.controller_id.as_str())
            })
            .collect();
        defenders.sort_unstable();
        defenders.dedup();
        for defender in defenders {
            let blockers: Vec<_> = self
                .battlefield(defender)
                .filter(|c| {
                    c.types.iter().any(|ty| ty == "Creature")
                        && !c.tapped
                        && !c.phased_out
                        && !c.text.to_ascii_lowercase().contains("can't block")
                })
                .collect();
            let attacks: Vec<_> = assignments
                .iter()
                .filter(|a| {
                    self.card(&a.target_id)
                        .map_or(a.target_id.as_str(), |c| c.controller_id.as_str())
                        == defender
                })
                .collect();
            let menu: Vec<_> = attacks
                .iter()
                .filter_map(|a| {
                    let card = self.card(&a.attacker_id)?;
                    Some(BlockableAttackerDto {
                        attacker_id: a.attacker_id.clone(),
                        valid_blocker_ids: blockers
                            .iter()
                            .filter(|b| self.visible_can_block(b, card))
                            .map(|b| b.id.clone())
                            .collect(),
                        min_blockers: if Self::has_keyword(card, "Menace") {
                            2
                        } else {
                            1
                        },
                        max_blockers: None,
                        must_be_blocked: false,
                    })
                })
                .collect();
            let ids: Vec<_> = blockers.iter().map(|c| c.id.clone()).collect();
            let blocks =
                self.declare_blockers_with_targets(&menu, &ids, defender, Some(assignments));
            for a in attacks {
                let Some(card) = self.card(&a.attacker_id) else {
                    continue;
                };
                let assigned: Vec<_> = blocks
                    .iter()
                    .filter(|b| b.attacker_id == a.attacker_id)
                    .filter_map(|b| self.card(&b.blocker_id))
                    .collect();
                let (hit, exchange, gain) = self.forecast_exchange(card, &assigned);
                let prevented = view
                    .players
                    .iter()
                    .find(|p| p.id == a.target_id)
                    .is_some_and(|p| {
                        p.player_keywords.iter().any(|keyword| {
                            let text = keyword.to_ascii_lowercase();
                            text.contains("protection")
                                && (text.contains("everything")
                                    || text.contains("creatures")
                                    || [
                                        ("white", 'W'),
                                        ("blue", 'U'),
                                        ("black", 'B'),
                                        ("red", 'R'),
                                        ("green", 'G'),
                                    ]
                                    .iter()
                                    .any(|(name, color)| {
                                        text.contains(name) && card.color.contains(*color)
                                    }))
                        })
                    });
                *damage.entry(a.target_id.clone()).or_default() += if prevented { 0 } else { hit };
                let gain = if prevented { (gain - hit).max(0) } else { gain };
                score += exchange
                    + gain.min((40 - self.life_of(me)).max(0)) * 2
                    + rewards.get(&card.id).copied().unwrap_or(0);
                if keep.contains(&a.attacker_id) {
                    score -= self.importance(card) * 2;
                }
            }
        }
        for (target, hit) in damage {
            if let Some(player) = view.players.iter().find(|p| p.id == target) {
                let hit = (hit - player.damage_prevention).max(0);
                let useful = hit.min(player.life.max(0));
                // Reachable lethal, after predicted blocks, outweighs exchange.
                if player.life > 0 && hit >= player.life {
                    score += 10_000;
                }
                score += useful * 3;
                if useful > 0 {
                    score += self.attack_target_score(&target, me).clamp(0, 200) / 20;
                }
            } else if let Some(card) = self.card(&target) {
                let loyalty = card.counters.get("Loyalty").copied().unwrap_or(0) as i32;
                score += hit.min(loyalty) * 4;
                if loyalty > 0 && hit >= loyalty {
                    score += self.importance(card);
                }
            }
        }
        score
    }

    pub(super) fn allocate_optional_attacks(
        &self,
        required: Vec<AttackAssignment>,
        attackers: &[AttackerOptionDto],
        me: &str,
        maximum: Option<usize>,
        limits: &HashMap<String, usize>,
        keep: &HashSet<String>,
    ) -> Vec<AttackAssignment> {
        // These board/text-derived values do not change between candidate
        // declarations; compute them once even for large token armies.
        let rewards: HashMap<_, _> = attackers
            .iter()
            .filter_map(|a| {
                self.card(&a.attacker_id)
                    .map(|c| (c.id.clone(), self.attack_reward(c, me)))
            })
            .collect();
        let optional: Vec<_> = attackers.iter().filter(|a| !a.must_attack).collect();
        let legal = |plan: &[AttackAssignment], a: &AttackerOptionDto, target: &str| {
            a.valid_target_ids.iter().any(|id| id == target)
                && !self.failed_attack_targets.contains(target)
                && maximum.is_none_or(|max| plan.len() < max)
                && limits
                    .get(target)
                    .is_none_or(|max| plan.iter().filter(|a| a.target_id == target).count() < *max)
        };
        let mut targets: Vec<_> = optional
            .iter()
            .flat_map(|a| a.valid_target_ids.iter())
            .cloned()
            .collect();
        targets.sort();
        targets.dedup();
        let mut seeds = vec![required.clone()];
        // Include coordinated attacks even when the first individual attacker
        // would die. A greedy sequence starting from no attack cannot find those.
        for target in &targets {
            let mut plan = required.clone();
            for a in &optional {
                if legal(&plan, a, target) {
                    plan.push(AttackAssignment {
                        attacker_id: a.attacker_id.clone(),
                        target_id: target.clone(),
                    });
                }
            }
            seeds.push(plan);
        }
        let mut best = required;
        let mut best_score = self.score_attack_plan(&best, me, keep, &rewards);
        for mut plan in seeds {
            let mut current = self.score_attack_plan(&plan, me, keep, &rewards);
            // Bounded coordinate improvements permit restraint, split targets,
            // and stopping at sufficient pressure instead of overcommitting.
            for _ in 0..2 {
                let mut changed = false;
                for a in &optional {
                    let without: Vec<_> = plan
                        .iter()
                        .filter(|p| p.attacker_id != a.attacker_id)
                        .cloned()
                        .collect();
                    let score = self.score_attack_plan(&without, me, keep, &rewards);
                    if score >= current && without.len() < plan.len() {
                        plan = without.clone();
                        current = score;
                        changed = true;
                    }
                    for target in &a.valid_target_ids {
                        if !legal(&without, a, target) {
                            continue;
                        }
                        let mut proposal = without.clone();
                        proposal.push(AttackAssignment {
                            attacker_id: a.attacker_id.clone(),
                            target_id: target.clone(),
                        });
                        let score = self.score_attack_plan(&proposal, me, keep, &rewards);
                        if score > current {
                            plan = proposal;
                            current = score;
                            changed = true;
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            if current > best_score || (current == best_score && plan.len() < best.len()) {
                best = plan;
                best_score = current;
            }
        }
        best
    }
}
