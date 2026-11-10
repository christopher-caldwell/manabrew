use super::*;
use crate::card::card_damage_history::TrackedEntity;

// Maximum number of required attackers that can be assigned to individually
// legal defenders without exceeding per-defender or global capacities. The
// augmenting-path match is exact for these capacity constraints; other joint
// restrictions remain the responsibility of declaration validation.
fn maximum_required_assignments(
    required: &[CardId], legal: &[(CardId, Vec<combat::DefenderId>)],
    defenders: &[combat::DefenderId], limits: &[(combat::DefenderId, usize)], global: Option<usize>,
) -> usize {
    let mut slots = Vec::new();
    for &defender in defenders {
        let cap = limits.iter().find(|(id, _)| *id == defender).map_or(required.len(), |(_, cap)| *cap);
        slots.extend(std::iter::repeat_n(defender, cap.min(required.len())));
    }
    fn augment(index: usize, required: &[CardId], legal: &[(CardId, Vec<combat::DefenderId>)],
               slots: &[combat::DefenderId], seen: &mut [bool], assigned: &mut [Option<usize>]) -> bool {
        let Some((_, choices)) = legal.iter().find(|(id, _)| *id == required[index]) else { return false; };
        for (slot, target) in slots.iter().enumerate() {
            if seen[slot] || !choices.contains(target) { continue; }
            seen[slot] = true;
            if assigned[slot].is_none() || augment(assigned[slot].unwrap(), required, legal, slots, seen, assigned) {
                assigned[slot] = Some(index);
                return true;
            }
        }
        false
    }
    let mut assigned = vec![None; slots.len()];
    let mut count = 0;
    for index in 0..required.len() {
        if augment(index, required, legal, &slots, &mut vec![false; slots.len()], &mut assigned) { count += 1; }
    }
    count.min(global.unwrap_or(required.len()))
}

// Search legal subsets with a bounded node budget. Never treat the capacity
// upper bound as achievable when joint restrictions have not been solved.
fn maximum_joint_required(
    required: &[CardId], legal: &[(CardId, Vec<combat::DefenderId>)],
    defenders: &[combat::DefenderId], limits: &[(combat::DefenderId, usize)],
    global: Option<usize>, cards: &[crate::card::Card],
) -> Result<Option<usize>, &'static str> {
    if !legal.iter().any(|(id, _)| !combat::attack_restriction::get_restrictions(&cards[id.index()]).is_empty()) { return Ok(None); }
    let upper = maximum_required_assignments(required, legal, defenders, limits, global);
    let mut best=0usize; let mut nodes=0usize; let mut selected=Vec::new();
    fn search(index: usize, selected: &mut Vec<CardId>, best: &mut usize, nodes: &mut usize,
        upper: usize, required: &[CardId], legal: &[(CardId, Vec<combat::DefenderId>)],
        defenders: &[combat::DefenderId], limits: &[(combat::DefenderId, usize)],
        global: Option<usize>, cards: &[crate::card::Card]) -> Result<(), &'static str> {
        if *best == upper { return Ok(()); }
        *nodes += 1; if *nodes > 100_000 { return Err("joint attack requirement search exceeded safe budget"); }
        if combat::attack_restriction::validate_attack_restrictions(selected, cards).is_empty()
            && maximum_required_assignments(selected,legal,defenders,limits,global) == selected.len() {
            *best=(*best).max(selected.iter().filter(|id| required.contains(id)).count());
            if *best == upper { return Ok(()); }
        }
        if index == legal.len() || selected.len() >= global.unwrap_or(legal.len()) { return Ok(()); }
        selected.push(legal[index].0);
        search(index+1,selected,best,nodes,upper,required,legal,defenders,limits,global,cards)?;
        selected.pop();
        search(index+1,selected,best,nodes,upper,required,legal,defenders,limits,global,cards)
    }
    search(0,&mut selected,&mut best,&mut nodes,upper,required,legal,defenders,limits,global,cards)?;
    Ok(Some(best))
}

impl GameLoop {
    pub fn step_combat(&mut self, game: &mut GameState, agents: &mut [Box<dyn PlayerAgent>]) {
        let _perf_scope =
            crate::perf::ParamsLookupScopeGuard::enter(crate::perf::ParamsLookupScope::Combat);
        let active = game.active_player();
        let defending = game.opponent_of(active);
        self.combat.clear_with_cards(&mut game.cards);
        game.turn.combat_block_assignments.clear();
        self.combat.attacking_player = Some(active);
        self.combat.defending_player = Some(defending);

        // Begin Combat
        self.set_phase(game, agents, PhaseType::CombatBegin);
        self.emit_phase_trigger(game, PhaseType::CombatBegin);
        self.step_with_priority(game, agents, false);
        if game.game_over {
            self.combat.clear_with_cards(&mut game.cards);
            return;
        }

        // EndCombatPhase (issue #22): if requested, exit combat early
        if game.end_combat_requested {
            game.end_combat_requested = false;
            self.combat.clear_with_cards(&mut game.cards);
            return;
        }

        // Recompute continuous effects before evaluating attack/block legality.
        // CantAttack / CantBlock flags are set here.
        apply_continuous_effects(game);
        self.trigger_handler.reset_active_triggers(game);

        // LKI: Snapshot battlefield state before combat declarations.
        // Mirrors Java's Game.copyLastState() called before declare attackers.
        game.copy_last_state();

        // Declare Attackers — freeze the stack during declarations.
        game.stack.freeze_stack();
        self.set_phase(game, agents, PhaseType::CombatDeclareAttackers);
        let available_attackers = combat::get_available_attackers(game, active);
        let possible_defenders = combat::get_possible_defenders(game, active);

        // Compute attack requirements (must-attack from statics + goad)
        let mut must_attackers = if available_attackers.is_empty() {
            Vec::new()
        } else {
            let requirements = combat::attack_requirement::compute_attack_requirements(
                &game.cards,
                &available_attackers,
                defending,
            );
            combat::attack_requirement::must_attack_ids(&requirements)
        };

        let goaded: Vec<_> = available_attackers.iter().filter_map(|&id|
            game.card(id).goaded_by.map(|player| (id, player))).collect();
        let legal: Vec<(CardId, Vec<combat::DefenderId>)> = available_attackers.iter().map(|&id| (id, possible_defenders.iter().copied()
            .filter(|&defender| combat::combat_util::can_attack_defender(game, id, defender)).collect())).collect();
        must_attackers.retain(|id| legal.iter().any(|(card, targets)| card == id && !targets.is_empty()));
        let prompt_max = crate::staticability::static_ability_attack_restrict::global_attack_restrict(&game.cards)
            .map(|max| max.max(0) as usize);
        let target_limits: Vec<(combat::DefenderId, usize)> = possible_defenders.iter().filter_map(|&target| {
            let combat::DefenderId::Player(player) = target else { return None; };
            crate::staticability::static_ability_attack_restrict::attack_restrict_num_for_defender(&game.cards, player)
                .map(|max| (target, max.max(0) as usize))
        }).collect();
        agents[active.index()].set_attack_requirements(&must_attackers, &goaded, &legal, prompt_max, &target_limits);

        let pass_until_skip = must_attackers.is_empty()
            && agents[active.index()]
                .get_pass_until()
                .is_some_and(|target| {
                    let reached = (active == target.player
                        && !game.turn.phase.is_before(target.phase))
                        || !game.player(target.player).is_alive();
                    !reached
                });

        // Java's PhaseHandler uses a do-while loop: declare attackers, validate,
        // and re-prompt if invalid.  We mirror this so RNG consumption matches.
        let mut chosen_attackers: Vec<(CardId, combat::DefenderId)> = Vec::new();
        if !available_attackers.is_empty() && !pass_until_skip {
            // Java parity: attacker declaration retries until a legal attack
            // set is found. A low cap can prematurely accept an invalid/no-attack
            // outcome on crowded boards (e.g. Silent Arbiter + MustAttack).
            // Keep a very high guard only as a last-resort safety valve.
            let max_attempts = 5000;
            for _attempt in 0..max_attempts {
                agents[active.index()].snapshot_state(game, &self.mana_pools);
                self.game_log.log(
                    GameLogEntryType::PriorityWaiting,
                    2,
                    format!(
                        "Waiting for {} attacker declaration",
                        game.player(active).name
                    ),
                );
                let agent = &mut agents[active.index()];
                let mut picked =
                    agent.choose_attackers(active, &available_attackers, &possible_defenders);
                if self.apply_pending_snapshot_restore(game, agents) {
                    return;
                }
                self.game_log.log(
                    GameLogEntryType::PriorityResponse,
                    2,
                    format!(
                        "{} declared {} attacker(s)",
                        game.player(active).name,
                        picked.len()
                    ),
                );

                // Validate attack restrictions (OnlyAlone, NotAlone, NeedGreaterPower, etc.)
                let attacker_ids: Vec<CardId> = picked.iter().map(|(a, _)| *a).collect();
                let illegal = combat::attack_restriction::validate_attack_restrictions(
                    &attacker_ids,
                    &game.cards,
                );
                if !illegal.is_empty() {
                    picked.retain(|(id, _)| !illegal.contains(id));
                }

                // Check AttackRestrict limits (global + per-defender).
                let global_max =
                    crate::staticability::static_ability_attack_restrict::global_attack_restrict(
                        &game.cards,
                    );

                // Global limit applies to ALL attackers regardless of defender.
                let mut invalid = false;
                if let Some(max) = global_max {
                    if picked.len() > max as usize {
                        invalid = true;
                    }
                }

                // Mirror Java's validateAttackers + countViolations + getLegalAttackers:
                // Count must-attack violations in the agent's raw declaration and compare
                // against the minimum violations achievable by the best legal attack.
                // If the agent's declaration has more violations, mark as invalid and retry
                // (matching Java's RNG consumption for the retry loop).
                if !invalid {
                    let current_violations = must_attackers
                        .iter()
                        .filter(|&&m| !picked.iter().any(|(a, _)| *a == m))
                        .count();
                    if current_violations > 0 {
                        // Count the maximum requirements satisfiable under
                        // individual target legality and capacity limits. Joint
                        // attacker restrictions are checked separately and can
                        // require further work beyond this capacity bound.
                        let max_attackers = maximum_joint_required(&must_attackers, &legal,
                            &possible_defenders, &target_limits, prompt_max, &game.cards)
                            .unwrap_or_else(|error| panic!("unsupported combat declaration: {error}"))
                            .unwrap_or_else(|| maximum_required_assignments(&must_attackers, &legal,
                                &possible_defenders, &target_limits, prompt_max));
                        let best_violations = must_attackers.len().saturating_sub(max_attackers);
                        if current_violations > best_violations {
                            invalid = true;
                        }
                    }
                }

                // Per-player limits do not apply to a planeswalker merely
                // because its controller is the protected player.
                if !invalid {
                    for &(target, max) in &target_limits {
                        if picked.iter().filter(|(_, defender)| *defender == target).count() > max {
                            invalid = true;
                            break;
                        }
                    }
                }

                if invalid {
                    // Declaration invalid — re-prompt like Java's PhaseHandler.
                    agents[active.index()].notify(
                        crate::agent::notification::GameNotification::Event(
                            crate::agent::GameLogEvent::warning("Attack declaration invalid"),
                        ),
                    );
                    continue;
                }

                chosen_attackers = picked;
                break;
            }
        }

        // Java parity: pre-mark declared attackers before optional attack-cost
        // resolution so they are not valid enlist targets.
        // Java does this by temporarily tapping non-vigilance attackers and
        // treating them as attacking before OptionalAttackCost is paid.
        let premarked_attackers: Vec<(CardId, combat::DefenderId)> = chosen_attackers.clone();
        for &(attacker_id, def) in &premarked_attackers {
            let defending_player = def.controlling_player(game);
            game.card_mut(attacker_id)
                .set_attacking_player(defending_player);
            if !game.card(attacker_id).has_vigilance() {
                game.card_mut(attacker_id).set_tapped(true);
            }
        }

        // Java parity: optional attack costs (Exert/Enlist) are chosen immediately
        // after attackers are declared and before CantAttackUnless payments.
        {
            let declared_attackers: Vec<CardId> = chosen_attackers
                .iter()
                .map(|(attacker, _)| *attacker)
                .collect();
            let mut optional_exert_by_attacker: crate::det::HashMap<
                CardId,
                Vec<(i32, String)>,
            > = crate::det::HashMap::default();
            let mut optional_enlist_by_attacker: crate::det::HashMap<
                CardId,
                Vec<(i32, String)>,
            > = crate::det::HashMap::default();

            for &attacker in &declared_attackers {
                let static_abilities = game.card(attacker).static_abilities.clone();
                for st in &static_abilities {
                    if !st.check_mode(&crate::staticability::StaticMode::OptionalAttackCost) {
                        continue;
                    }
                    let Some(cost_raw) = st.ir.cost.as_deref() else {
                        continue;
                    };
                    let parsed = crate::cost::parse_cost(cost_raw);
                    for part in parsed.parts {
                        match part {
                            crate::cost::CostPart::Exert {
                                amount,
                                type_filter,
                            } => {
                                let amount_n = amount.resolve(game, attacker, active);
                                optional_exert_by_attacker
                                    .entry(attacker)
                                    .or_default()
                                    .push((amount_n, type_filter));
                            }
                            crate::cost::CostPart::Enlist {
                                amount,
                                type_filter,
                            } => {
                                let amount_n = amount.resolve(game, attacker, active);
                                optional_enlist_by_attacker
                                    .entry(attacker)
                                    .or_default()
                                    .push((amount_n, type_filter));
                            }
                            _ => {}
                        }
                    }
                }
            }

            let possible_exerters: Vec<CardId> = declared_attackers
                .iter()
                .copied()
                .filter(|cid| optional_exert_by_attacker.contains_key(cid))
                .collect();
            if !possible_exerters.is_empty() {
                let chosen = agents[active.index()].exert_attackers(active, &possible_exerters);
                for attacker in chosen {
                    // Exert is paid unconditionally once chosen via exert_attackers
                    // (mirrors HumanPlay.payCostDuringAbilityResolve's CostExert case).
                    if let Some(parts) = optional_exert_by_attacker.get(&attacker).cloned() {
                        for (resolved, type_filter) in parts {
                            if resolved > 0 {
                                self.pay_exert_cost(
                                    game,
                                    agents,
                                    active,
                                    attacker,
                                    &type_filter,
                                    resolved,
                                );
                            }
                        }
                    }
                }
            }

            // Re-check enlist targets AFTER exert loop — exerting taps creatures,
            // which can invalidate enlist candidates.
            let enlist_can_pay = !crate::cost::get_enlist_targets(game, active).is_empty();
            let possible_enlisters: Vec<CardId> = if enlist_can_pay {
                declared_attackers
                    .iter()
                    .copied()
                    .filter(|cid| optional_enlist_by_attacker.contains_key(cid))
                    .collect()
            } else {
                Vec::new()
            };

            if !possible_enlisters.is_empty() {
                let chosen = agents[active.index()].enlist_attackers(active, &possible_enlisters);
                for attacker in chosen {
                    if let Some(parts) = optional_enlist_by_attacker.get(&attacker).cloned() {
                        for (resolved, type_filter) in parts {
                            if resolved > 0 {
                                self.pay_enlist_cost(
                                    game,
                                    agents,
                                    active,
                                    attacker,
                                    &type_filter,
                                    resolved,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Check attack costs (Propaganda, Ghostly Prison effects)
        {
            let mut cost_failures = Vec::new();
            for &(attacker_id, defender) in &chosen_attackers {
                let cost = combat::attack_cost::get_attack_cost(
                    &game.cards,
                    game.card(attacker_id),
                    defender,
                );
                if cost > 0 {
                    let controller = game.card(attacker_id).controller;
                    let attacker_name = game.card(attacker_id).card_name.clone();
                    let description = format!("Pay {{{cost}}} to attack with {attacker_name}");

                    // A declined mana activation is unavailable until another action changes the payment state.
                    let mut rejected = crate::det::HashSet::default();
                    let mut invalid_actions = 0;
                    loop {
                        let mut tappable_lands = self.get_tappable_lands(game, controller);
                        let pool_snapshot = self.pool(controller).clone();
                        let untappable_lands =
                            self.get_untappable_lands(game, controller, &pool_snapshot);
                        let pool_total = self.pool(controller).total_mana();
                        let mut mana_payment_sources =
                            crate::mana::collect_mana_payment_sources(game, controller, &[]);
                        mana_payment_sources.mana_ability_options.retain(|option|
                            !rejected.contains(&(option.card_id, option.ability_index)));
                        tappable_lands.retain(|id| {
                            !rejected.iter().any(|(source, _)| source == id)
                                || mana_payment_sources.mana_ability_options.iter().any(|option| option.card_id == *id)
                        });

                        agents[controller.index()].snapshot_state(game, &self.mana_pools);
                        let action = agents[controller.index()].pay_combat_cost(
                            controller,
                            attacker_id,
                            cost,
                            &description,
                            &mana_payment_sources.mana_ability_options,
                            &tappable_lands,
                            &untappable_lands,
                            pool_total,
                        );

                        match action {
                            CombatCostAction::TapLand {
                                card_id: land_id,
                                mana_ability_index,
                                express_choice,
                            } => {
                                if !tappable_lands.contains(&land_id)
                                    || mana_ability_index.is_some_and(|idx| rejected.contains(&(land_id, idx))) {
                                    invalid_actions += 1;
                                    if invalid_actions > 3 { cost_failures.push(attacker_id); break; }
                                    continue;
                                }
                                let undo_record =
                                    self.begin_mana_undo_action(game, controller, land_id);
                                let pool_snapshot = self.pool(controller).begin_tap_tracking();
                                // Use actual mana ability when available
                                let mana_ab = {
                                    let c = game.card(land_id);
                                    if let Some(requested_idx) = mana_ability_index {
                                        c.activated_abilities
                                            .iter()
                                            .find(|ab| {
                                                ab.is_mana_ability
                                                    && ab.ability_index == requested_idx
                                            })
                                            .cloned()
                                    } else {
                                        c.activated_abilities
                                            .iter()
                                            .find(|ab| ab.is_mana_ability && !rejected.contains(&(land_id, ab.ability_index)))
                                            .cloned()
                                    }
                                };
                                if let Some(ab) = mana_ab {
                                    let resolved = self.with_shared_state_mutation(
                                        game,
                                        agents,
                                        |this, game, agents| {
                                            this.resolve_mana_ability(
                                                game,
                                                agents,
                                                controller,
                                                land_id,
                                                &ab,
                                                express_choice,
                                            )
                                        },
                                    );
                                    if resolved {
                                        rejected.clear();
                                        invalid_actions = 0;
                                    } else {
                                        rejected.insert((land_id, ab.ability_index));
                                    }
                                } else {
                                    let atom_opt = {
                                        let c = game.card(land_id);
                                        if c.is_land() && !c.tapped {
                                            basic_land_mana_atom(c)
                                        } else {
                                            None
                                        }
                                    };
                                    if let Some(atom) = atom_opt {
                                        rejected.clear();
                                        invalid_actions = 0;
                                        game.tap(land_id);
                                        self.pool_mut(controller).add(atom, 1);
                                        self.trigger_handler.run_trigger(
                                            TriggerType::Taps,
                                            RunParams {
                                                card: Some(land_id),
                                                player: Some(controller),
                                                ..Default::default()
                                            },
                                            false,
                                        );
                                        self.trigger_handler.run_trigger(
                                            TriggerType::TapsForMana,
                                            RunParams {
                                                card: Some(land_id),
                                                player: Some(controller),
                                                ..Default::default()
                                            },
                                            false,
                                        );
                                    }
                                }
                                let produced =
                                    self.pool(controller).end_tap_tracking(&pool_snapshot);
                                self.finish_mana_undo_action(undo_record, produced.len());
                            }
                            CombatCostAction::UntapLand(land_id) => {
                                if !untappable_lands.contains(&land_id) {
                                    continue;
                                }
                                if self.undo_mana_action(game, controller, land_id) {
                                    rejected.clear();
                                    invalid_actions = 0;
                                }
                            }
                            CombatCostAction::AutoPay => {
                                self.invalidate_mana_undo_for_player(controller);
                                let game_ptr: *mut GameState = game;
                                let trigger_ptr = std::ptr::from_mut(&mut self.trigger_handler);
                                let mut callback = Self::make_mana_payment_callback(
                                    trigger_ptr, game_ptr, agents, controller, attacker_id,
                                );
                                let result = crate::mana::pay_mana_cost_auto_with_callback(
                                    game, self.pool_mut(controller), controller,
                                    &forge_foundation::ManaCost::generic(cost), None, 0,
                                    &crate::mana::ManaPaymentContext::default(), false, &mut callback,
                                );
                                drop(callback);
                                if let Some(result) = result {
                                    self.resolve_auto_tapped_mana_sub_abilities(game, agents, controller, &result.choices);
                                    if result.cancelled { cost_failures.push(attacker_id); }
                                } else {
                                    cost_failures.push(attacker_id);
                                }
                                break;
                            }
                            CombatCostAction::Pay => {
                                self.invalidate_mana_undo_for_player(controller);
                                let pool = &mut self.mana_pools[controller.index()];
                                if pool.total_mana() >= cost {
                                    pool.spend_generic(cost);
                                    // Successfully paid
                                } else {
                                    // Not enough mana — treat as decline
                                    cost_failures.push(attacker_id);
                                }
                                break;
                            }
                            CombatCostAction::Decline => {
                                self.invalidate_mana_undo_for_player(controller);
                                cost_failures.push(attacker_id);
                                break;
                            }
                        }
                    }
                }
            }
            chosen_attackers.retain(|(id, _)| !cost_failures.contains(id));
        }

        // Undo temporary attack markers for attackers removed by cost payment.
        for &(attacker_id, _) in &premarked_attackers {
            if !chosen_attackers.iter().any(|(id, _)| *id == attacker_id) {
                game.card_mut(attacker_id).clear_attacking_player();
                if !game.card(attacker_id).has_vigilance() {
                    game.card_mut(attacker_id).set_tapped(false);
                }
            }
        }

        if !chosen_attackers.is_empty() {
            crate::agent::notify_all_agents(
                agents,
                crate::agent::GameLogEvent::action("Combat phase begins").with_player(active),
            );
            let attackers_msg = chosen_attackers
                .iter()
                .map(|(attacker_id, defender)| {
                    let attacker_name = game.card(*attacker_id).card_name.clone();
                    let defender_name = match defender {
                        combat::DefenderId::Player(pid) => game.player(*pid).name.clone(),
                        combat::DefenderId::Permanent(cid) => game.card(*cid).card_name.clone(),
                    };
                    format!("{attacker_name} -> {defender_name}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            crate::agent::notify_all_agents(
                agents,
                crate::agent::GameLogEvent::action(format!("Attackers: {attackers_msg}"))
                    .with_player(active),
            );
        }

        // Tap attackers (Vigilance skips tapping)
        let num_attackers = chosen_attackers.len() as i32;
        game.player_attack_combat_reset(active);
        for &(attacker_id, defender) in &chosen_attackers {
            if !game.card(attacker_id).has_vigilance() {
                // We pre-tapped attackers before OptionalAttackCost resolution to
                // mirror Java legality checks; untap first so this tap emits the
                // declaration-time Taps trigger once.
                if game.card(attacker_id).tapped {
                    game.untap(attacker_id);
                }
                game.tap(attacker_id);
                // Java attacker.tap(...) emits Taps triggers when a creature becomes tapped
                // as part of attacker declaration.
                self.trigger_handler.run_trigger(
                    TriggerType::Taps,
                    RunParams {
                        card: Some(attacker_id),
                        player: Some(active),
                        ..Default::default()
                    },
                    false,
                );
            }
            game.card_mut(attacker_id).mark_attacked_this_turn();
            // Set attacking_player to the controlling player of the defender
            let def_player = defender.controlling_player(game);
            game.card_mut(attacker_id).set_attacking_player(def_player);
            self.combat.declare_attacker(
                attacker_id,
                defender,
                game.card(attacker_id).zone_timestamp,
            );

            // Record attack in damage history
            game.card_mut(attacker_id)
                .damage_history
                .record_attack(num_attackers - 1);
            game.card_mut(attacker_id)
                .damage_history
                .set_creature_attacked_this_combat(
                    Some(match defender {
                        combat::DefenderId::Player(pid) => TrackedEntity::Player(pid),
                        combat::DefenderId::Permanent(cid) => TrackedEntity::Card(cid),
                    }),
                    num_attackers - 1,
                    matches!(defender, combat::DefenderId::Permanent(_)),
                );
            if let combat::DefenderId::Player(pid) = defender {
                if !game
                    .player(active)
                    .attacked_players_this_turn
                    .contains(&pid)
                {
                    game.player_record_attacked_player(active, pid);
                }
                if !game
                    .player(active)
                    .attacked_players_this_combat
                    .contains(&pid)
                {
                    game.player_record_attacked_player(active, pid);
                }
            }

            crate::ability::effects::ring_tempts_you_effect::sync_ring_effect(
                game,
                &mut self.trigger_handler,
                active,
            );

            // Fire Attacks trigger for each attacker
            self.trigger_handler.run_trigger(
                TriggerType::Attacks,
                RunParams {
                    attacker: Some(attacker_id),
                    card: Some(attacker_id),
                    defending_player: Some(def_player),
                    num_attackers: Some(num_attackers as usize),
                    ..Default::default()
                },
                false,
            );
        }
        // Fire AttackersDeclaredOneTarget-style batches first, then the aggregate event.
        if !chosen_attackers.is_empty() {
            let mut grouped_attackers: crate::det::HashMap<combat::DefenderId, Vec<CardId>> =
                crate::det::HashMap::default();
            for &(attacker_id, defender) in &chosen_attackers {
                grouped_attackers
                    .entry(defender)
                    .or_default()
                    .push(attacker_id);
            }
            let mut attacked_player_ids = Vec::new();
            let mut attacked_card_ids = Vec::new();
            for (defender, attackers) in &grouped_attackers {
                let mut params = RunParams {
                    attacker_ids: Some(attackers.clone()),
                    player: Some(game.active_player()),
                    attacking_player: Some(game.active_player()),
                    ..Default::default()
                };
                match defender {
                    combat::DefenderId::Player(pid) => {
                        params.attacked_player = Some(*pid);
                        params.defenders_player_ids = Some(vec![*pid]);
                        attacked_player_ids.push(*pid);
                    }
                    combat::DefenderId::Permanent(cid) => {
                        params.attacked_card = Some(*cid);
                        params.defenders_card_ids = Some(vec![*cid]);
                        attacked_card_ids.push(*cid);
                    }
                }
                self.trigger_handler.run_trigger(
                    TriggerType::AttackersDeclaredOneTarget,
                    params,
                    false,
                );
            }

            let attacker_ids: Vec<CardId> = chosen_attackers.iter().map(|(a, _)| *a).collect();
            self.trigger_handler.run_trigger(
                TriggerType::AttackersDeclared,
                RunParams {
                    player: Some(game.active_player()),
                    attacking_player: Some(game.active_player()),
                    attacker_ids: Some(attacker_ids),
                    defenders_player_ids: if attacked_player_ids.is_empty() {
                        None
                    } else {
                        Some(attacked_player_ids)
                    },
                    defenders_card_ids: if attacked_card_ids.is_empty() {
                        None
                    } else {
                        Some(attacked_card_ids)
                    },
                    ..Default::default()
                },
                false,
            );
        }
        // Recompute continuous effects now that `attacking_player` is set on
        // declared attackers.  This allows effects like Watchdog's
        // "Affected$ Creature.attackingYou | AddPower$ -1" to apply correctly.
        apply_continuous_effects(game);
        self.trigger_handler.reset_active_triggers(game);
        // Unfreeze the stack now that attackers are declared.
        game.stack.unfreeze_stack();
        // Java parity: PhaseHandler sets givePriorityToPlayer = inCombat() after
        // declare attackers. In Java, inCombat() returns `combat != null` (true
        // whenever the combat object exists, regardless of whether attackers were
        // declared), so priority is always given here.
        self.step_with_priority(game, agents, false);
        if game.game_over {
            self.combat.clear_with_cards(&mut game.cards);
            return;
        }

        // Java parity: PhaseHandler still advances into COMBAT_DECLARE_BLOCKERS
        // even when it will be skipped due to no attackers.
        self.combat.remove_absent_combatants(&mut game.cards);
        if !self.combat.has_attackers() {
            self.set_phase(game, agents, PhaseType::CombatDeclareBlockers);
        }
        if self.combat.has_attackers() {
            // Each attacked player declares blockers for attackers aimed at
            // that player or a permanent they defend, in turn order.
            game.stack.freeze_stack();
            self.set_phase(game, agents, PhaseType::CombatDeclareBlockers);
            let order = &game.player_order;
            let start = order.iter().position(|&player| player == active).unwrap_or(0);
            let defenders: Vec<_> = (1..=order.len()).map(|offset| order[(start + offset) % order.len()])
                .filter(|&player| self.combat.attackers.iter().any(|(_, defender)| defender.controlling_player(game) == player)).collect();
            for defending in defenders {
                {
                    use crate::replacement::replacement_handler::{apply_replacements, ReplacementEvent};
                    let mut event = ReplacementEvent::DeclareBlocker { player: defending };
                    let _ = apply_replacements(game, &mut event);
                }
                let attacker_card_ids: Vec<CardId> = self.combat.attackers.iter()
                    .filter(|(_, defender)| defender.controlling_player(game) == defending)
                    .map(|(attacker, _)| *attacker).collect();
            let available_blockers = combat::get_available_blockers(game, defending);
            let legal_blockers =
                combat::filter_legal_blockers(game, &attacker_card_ids, &available_blockers);
            let has_any_legal_blocker = !legal_blockers.is_empty();

            if has_any_legal_blocker {
                agents[defending.index()].snapshot_state(game, &self.mana_pools);
                self.game_log.log(
                    GameLogEntryType::PriorityWaiting,
                    2,
                    format!(
                        "Waiting for {} blocker declaration",
                        game.player(defending).name
                    ),
                );
                let max_blockers = {
                    let raw =
                        crate::staticability::static_ability_block_restrict::block_restrict_num(
                            &game.cards,
                            defending,
                        );
                    if raw < i32::MAX {
                        Some(raw as usize)
                    } else {
                        None
                    }
                };
                let mut chosen_blockers = {
                    let def_agent = &mut agents[defending.index()];
                    let legality: Vec<_> = attacker_card_ids.iter().map(|&attacker| {
                        let blockers = legal_blockers.iter().copied()
                            .filter(|&blocker| combat::can_creature_block(game, blocker, attacker))
                            .collect();
                        (attacker, blockers, combat::combat_util::get_min_num_blockers_for_attacker(game, attacker))
                    }).collect();
                    def_agent.choose_blockers_with_legality(
                        defending,
                        &attacker_card_ids,
                        &legal_blockers,
                        max_blockers,
                        &legality,
                    )
                };
                if self.apply_pending_snapshot_restore(game, agents) {
                    return;
                }
                // Ignore duplicate blocker assignments; first assignment wins.
                let mut seen_blockers = crate::det::HashSet::default();
                chosen_blockers.retain(|(blocker, _)| seen_blockers.insert(*blocker));
                self.game_log.log(
                    GameLogEntryType::PriorityResponse,
                    2,
                    format!(
                        "{} declared {} blocker assignment(s)",
                        game.player(defending).name,
                        chosen_blockers.len()
                    ),
                );

                for (blocker, attacker) in chosen_blockers.into_iter() {
                    // Validate: use comprehensive evasion check
                    if !available_blockers.contains(&blocker) || !attacker_card_ids.contains(&attacker)
                        || !combat::can_creature_block(game, blocker, attacker) {
                        continue; // illegal block
                    }
                    self.combat.declare_blocker(
                        blocker,
                        attacker,
                        game.card(blocker).zone_timestamp,
                    );

                }

                // Block cost checking (War Cadence effects)
                {
                    let mut block_cost_failures = Vec::new();
                    for &(blocker_id, attacker_id) in &self.combat.blockers {
                        if game.card(blocker_id).controller != defending { continue; }
                        let cost = combat::block_cost::get_block_cost(
                            &game.cards,
                            game.card(blocker_id),
                            game.card(attacker_id),
                        );
                        if cost > 0 {
                            let controller = game.card(blocker_id).controller;
                            let pool = &mut self.mana_pools[controller.index()];
                            if pool.total_mana() >= cost {
                                pool.spend_generic(cost);
                            } else {
                                block_cost_failures.push(blocker_id);
                            }
                        }
                    }
                    self.combat
                        .blockers
                        .retain(|(b, _)| !block_cost_failures.contains(b));
                }

                // Block validation (Menace, can't block alone)
                let invalid_blocks = combat::validate_blocks(game, &self.combat);
                for (blocker_id, attacker_id) in &invalid_blocks {
                    self.combat
                        .blockers
                        .retain(|(b, a)| !(b == blocker_id && a == attacker_id));
                }

                // Must-block enforcement: auto-assign blockers to required targets
                let all_legal_blockers: Vec<CardId> = available_blockers.clone();
                for &blocker_id in &all_legal_blockers {
                    let must_targets: Vec<_> = combat::compute_must_block_targets(game, &self.combat, blocker_id)
                        .into_iter().filter(|attacker| attacker_card_ids.contains(attacker)).collect();
                    if must_targets.is_empty() {
                        continue;
                    }
                    let currently_blocking: Vec<CardId> = self
                        .combat
                        .blockers
                        .iter()
                        .filter(|(b, _)| *b == blocker_id)
                        .map(|(_, a)| *a)
                        .collect();
                    if !must_targets.iter().any(|t| currently_blocking.contains(t)) {
                        // Not blocking any required target — force-assign first
                        if combat::can_creature_block(game, blocker_id, must_targets[0]) {
                            self.combat.declare_blocker(
                                blocker_id,
                                must_targets[0],
                                game.card(blocker_id).zone_timestamp,
                            );
                        }
                    }
                }

                // Record damage history for blockers
                for &(blocker_id, attacker_id) in &self.combat.blockers {
                    if game.card(blocker_id).controller != defending { continue; }
                    self.trigger_handler.run_trigger(TriggerType::Blocks, RunParams {
                        blocker: Some(blocker_id), blocked_attacker: Some(attacker_id), card: Some(blocker_id), ..Default::default()
                    }, false);
                    game.card_mut(blocker_id).damage_history.record_block();
                    game.card_mut(attacker_id)
                        .damage_history
                        .record_got_blocked();
                }

                // Publish finalized blocker assignments for UI snapshots in this combat.
                game.turn.combat_block_assignments = self.combat.blockers.clone();

                if !self.combat.blockers.is_empty() {
                    let blockers_msg = self
                        .combat
                        .blockers
                        .iter()
                        .map(|(blocker_id, attacker_id)| {
                            let blocker_name = game.card(*blocker_id).card_name.clone();
                            let attacker_name = game.card(*attacker_id).card_name.clone();
                            format!("{blocker_name} -> {attacker_name}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    crate::agent::notify_all_agents(
                        agents,
                        crate::agent::GameLogEvent::action(format!("Blockers: {blockers_msg}"))
                            .with_player(defending),
                    );
                }
            }

            }

            // Auto-order blockers by declaration order — Java's parity
            // harness sets `legacyOrderCombatants = false` so the agent is
            // never prompted (Combat.java:494). Mirror that to keep the RNG
            // and trace aligned with Java.
            for &(attacker_id, _) in &self.combat.attackers.clone() {
                let blockers_for = self.combat.get_blockers_for(attacker_id);
                if blockers_for.len() > 1 {
                    self.combat.damage_order.insert(attacker_id, blockers_for);
                }
            }

            // Unfreeze the stack now that blockers are declared.
            game.stack.unfreeze_stack();

            // Fire BlockersDeclared batch trigger before the priority
            // window so these triggers are on the stack when players
            // receive priority (CR 509.4). Mirrors Java's
            // declareBlockersTurnBasedAction() which fires all block
            // triggers before mainLoopStep() gives priority.
            self.trigger_handler.run_trigger(
                TriggerType::BlockersDeclared,
                RunParams {
                    blocker_ids: Some(self.combat.blockers.iter().map(|(b, _)| *b).collect()),
                    ..Default::default()
                },
                false,
            );

            // Fire AttackerBlocked / AttackerUnblocked triggers
            for &(attacker_id, defender_id) in &self.combat.attackers.clone() {
                if self.combat.is_blocked(attacker_id) {
                    let blockers_for = self.combat.get_blockers_for(attacker_id);
                    self.trigger_handler.run_trigger(
                        TriggerType::AttackerBlocked,
                        RunParams {
                            attacker: Some(attacker_id),
                            card: Some(attacker_id),
                            defending_player: Some(defender_id.controlling_player(game)),
                            ..Default::default()
                        },
                        false,
                    );
                    self.trigger_handler.run_trigger(
                        TriggerType::AttackerBlockedOnce,
                        RunParams {
                            attacker: Some(attacker_id),
                            card: Some(attacker_id),
                            blocker_ids: Some(blockers_for.clone()),
                            defending_player: Some(defender_id.controlling_player(game)),
                            ..Default::default()
                        },
                        false,
                    );
                    for blocker_id in blockers_for {
                        self.trigger_handler.run_trigger(
                            TriggerType::AttackerBlockedByCreature,
                            RunParams {
                                attacker: Some(attacker_id),
                                card: Some(attacker_id),
                                blocker: Some(blocker_id),
                                blocked_attacker: Some(attacker_id),
                                defending_player: Some(defender_id.controlling_player(game)),
                                ..Default::default()
                            },
                            false,
                        );
                    }
                } else {
                    self.trigger_handler.run_trigger(
                        TriggerType::AttackerUnblocked,
                        RunParams {
                            attacker: Some(attacker_id),
                            card: Some(attacker_id),
                            ..Default::default()
                        },
                        false,
                    );
                    self.trigger_handler.run_trigger(
                        TriggerType::AttackerUnblockedOnce,
                        RunParams {
                            attacker: Some(attacker_id),
                            card: Some(attacker_id),
                            ..Default::default()
                        },
                        false,
                    );
                }
            }

            game.turn.combat_blockers_declared = true;
            self.step_with_priority(game, agents, false);
            if game.game_over {
                self.combat.clear_with_cards(&mut game.cards);
                game.turn.combat_block_assignments.clear();
                return;
            }
        }

        // Java parity: combatants may leave/re-enter during declare blockers
        // priority (e.g. sacrificing an attacker). Re-prune before damage.
        self.combat.remove_absent_combatants(&mut game.cards);

        // Pre-populate LKI cache for all combat participants so that if a
        // creature dies during damage, its combat role is already recorded.
        for &(attacker_id, _) in &self.combat.attackers.clone() {
            self.combat.save_lki(attacker_id);
        }
        for &(blocker_id, _) in &self.combat.blockers.clone() {
            self.combat.save_lki(blocker_id);
        }

        self.set_phase(game, agents, PhaseType::CombatFirstStrikeDamage);
        self.combat.remove_absent_combatants(&mut game.cards);
        if self.combat.has_attackers() {
            // LKI: Snapshot battlefield state before first strike damage.
            // Mirrors Java's Game.copyLastState() called before damage resolution.
            game.copy_last_state();

            let fs_unblocked_choices = self.choose_assign_as_unblocked(game, agents, true);
            let fs_life_lost_before: Vec<i32> = game.players.iter().map(|p| p.life_lost_this_turn).collect();
            let combat::CombatDamageResolution {
                events: fs_events,
                counter_table: fs_counter_table,
            } = self
                .combat
                .resolve_damage_step(game, agents, true, &fs_unblocked_choices);
            // Record damage in source damage history for player-targeted combat damage
            for event in &fs_events {
                if event.target_player.is_some() && event.amount > 0 {
                    game.card_mut(event.source)
                        .damage_history
                        .record_damage(event.amount, true);
                }
            }
            let fs_damage_assigned = !fs_events.is_empty();
            self.fire_combat_damage_triggers(&fs_events);
            for (index, before) in fs_life_lost_before.iter().enumerate() {
                let player = crate::ids::PlayerId(index as u32);
                let lost = game.player(player).life_lost_this_turn - before;
                if lost > 0 {
                    let params = RunParams {
                        player: Some(player), life_amount: Some(lost),
                        first_time: Some(game.player(player).life_lost_this_turn == lost),
                        ..Default::default()
                    };
                    self.trigger_handler.run_trigger(TriggerType::LifeLost, params.clone(), false);
                    self.trigger_handler.run_trigger(TriggerType::LifeLostAll, params, false);
                }
            }
            fs_counter_table.replace_counter_effect(
                game,
                Some(&mut self.trigger_handler),
                Some(agents),
                None,
                false,
                Default::default(),
            );
            // Flush triggers before SBA so that triggers from creatures about
            // to die (e.g. enrage) are matched while still on the battlefield.
            self.trigger_handler.flush_waiting_triggers(game);
            // Java parity: skip priority when no first-strike damage assigned.
            if fs_damage_assigned {
                self.step_with_priority(game, agents, false);
            }
            if game.game_over {
                self.combat.clear_with_cards(&mut game.cards);
                game.turn.combat_block_assignments.clear();
                return;
            }
        }

        self.set_phase(game, agents, PhaseType::CombatDamage);
        self.combat.remove_absent_combatants(&mut game.cards);
        if self.combat.has_attackers() {
            // Run AssignDealDamage replacement effects for each attacker.
            {
                use crate::replacement::replacement_handler::{
                    apply_replacements, ReplacementEvent,
                };
                let attacker_ids: Vec<CardId> =
                    self.combat.attackers.iter().map(|(a, _)| *a).collect();
                for &attacker_id in &attacker_ids {
                    let mut event = ReplacementEvent::AssignDealDamage { card: attacker_id };
                    apply_replacements(game, &mut event);
                }
            }

            // LKI: Snapshot battlefield state before combat damage.
            // Mirrors Java's Game.copyLastState() called before damage resolution.
            game.copy_last_state();

            let unblocked_choices = self.choose_assign_as_unblocked(game, agents, false);
            let dmg_life_lost_before: Vec<i32> = game.players.iter().map(|p| p.life_lost_this_turn).collect();
            let combat::CombatDamageResolution {
                events: dmg_events,
                counter_table: damage_counter_table,
            } = self
                .combat
                .resolve_damage_step(game, agents, false, &unblocked_choices);
            // Record damage in source damage history for player-targeted combat damage
            for event in &dmg_events {
                if event.target_player.is_some() && event.amount > 0 {
                    game.card_mut(event.source)
                        .damage_history
                        .record_damage(event.amount, true);
                }
            }
            // Java parity: skip priority when no damage was actually assigned
            // (e.g. 0-power attackers). Mirrors PhaseHandler.java lines 335-343
            // where assignCombatDamage returns false → givePriorityToPlayer = false.
            let damage_assigned = !dmg_events.is_empty();
            if damage_assigned {
                self.notify_state_changed(game, agents);
            }
            self.fire_combat_damage_triggers(&dmg_events);
            for (index, before) in dmg_life_lost_before.iter().enumerate() {
                let player = crate::ids::PlayerId(index as u32);
                let lost = game.player(player).life_lost_this_turn - before;
                if lost > 0 {
                    let params = RunParams {
                        player: Some(player), life_amount: Some(lost),
                        first_time: Some(game.player(player).life_lost_this_turn == lost),
                        ..Default::default()
                    };
                    self.trigger_handler.run_trigger(TriggerType::LifeLost, params.clone(), false);
                    self.trigger_handler.run_trigger(TriggerType::LifeLostAll, params, false);
                }
            }
            damage_counter_table.replace_counter_effect(
                game,
                Some(&mut self.trigger_handler),
                Some(agents),
                None,
                false,
                Default::default(),
            );
            // Flush triggers before SBA so that triggers from creatures about
            // to die (e.g. enrage) are matched while still on the battlefield.
            self.trigger_handler.flush_waiting_triggers(game);
            if damage_assigned {
                self.step_with_priority(game, agents, false);
            }
            if game.game_over {
                self.combat.clear_with_cards(&mut game.cards);
                game.turn.combat_block_assignments.clear();
                return;
            }
        }

        // End combat
        self.set_phase(game, agents, PhaseType::CombatEnd);
        self.emit_phase_trigger(game, PhaseType::CombatEnd);
        // Revert any `ControlGain$ LoseControl$ EndOfCombat` steals (Threaten-
        // style "attack and return").
        crate::ability::effects::control_gain_effect::end_of_combat_hook(game);
        self.step_with_priority(game, agents, false);

        // End-of-combat damage history reset and must_block cleanup
        for card in game.cards.iter_mut() {
            if card.zone == ZoneType::Battlefield && card.is_creature() {
                card.damage_history.end_combat();
                card.must_block = false;
                card.must_block_cards.clear();
            }
        }

        self.combat.clear_with_cards(&mut game.cards);
        game.turn.combat_block_assignments.clear();
        // Recompute continuous effects after combat ends so that stale
        // combat-dependent modifiers (e.g. Watchdog's "creatures attacking you
        // get -1/-0") are cleared.  Without this, static_power_modifier lingers
        // until the next apply_continuous_effects call, causing snapshot drift.
        apply_continuous_effects(game);
        self.trigger_handler.reset_active_triggers(game);
    }

    fn choose_assign_as_unblocked(
        &mut self,
        game: &GameState,
        agents: &mut [Box<dyn PlayerAgent>],
        first_strike_only: bool,
    ) -> crate::det::HashSet<CardId> {
        let mut choices = crate::det::HashSet::default();
        for &(attacker_id, _) in &self.combat.attackers {
            if !self.combat.is_blocked(attacker_id) {
                continue;
            }
            let attacker = game.card(attacker_id);
            let has_fs = attacker.has_first_strike();
            let has_ds = attacker.has_double_strike();
            let deals_in_step = if first_strike_only {
                has_fs || has_ds
            } else {
                !has_fs || has_ds
            };
            if !deals_in_step {
                continue;
            }
            if !crate::staticability::static_ability_assign_combat_damage_as_unblocked::has_optional_assign_as_unblocked(
                &game.cards,
                attacker,
            ) {
                continue;
            }

            let controller = attacker.controller;
            let desc = format!(
                "Have {} assign combat damage as though unblocked?",
                attacker.card_name
            );
            agents[controller.index()].snapshot_state(game, &self.mana_pools);
            if agents[controller.index()].choose_optional_trigger(
                controller,
                &desc,
                Some(attacker_id),
                None,
            ) {
                choices.insert(attacker_id);
            }
        }
        choices
    }
}

#[cfg(test)]
mod scoped_target_limit_regression {
    use super::*;
    #[test]
    fn exact_matching_handles_competing_and_alternative_defenders() {
        let a = CardId(0); let b = CardId(1);
        let x = combat::DefenderId::Player(PlayerId(1));
        let y = combat::DefenderId::Player(PlayerId(2));
        let required = vec![a, b]; let limits = vec![(x, 1)];
        let flexible = vec![(a, vec![x, y]), (b, vec![x])];
        assert_eq!(maximum_required_assignments(&required, &flexible, &[x, y], &limits, None), 2);
        assert_eq!(maximum_required_assignments(&required, &flexible, &[x, y], &limits, Some(1)), 1);
        let one_target = vec![(a, vec![x]), (b, vec![x])];
        assert_eq!(maximum_required_assignments(&required, &one_target, &[x], &limits, None), 1);
    }
}

#[cfg(test)]
mod large_joint_regression {
    use super::*;
    use forge_carddb::parse_card_script;
    use forge_foundation::ZoneType;
    #[test]
    fn companion_requirement_respects_global_cap_at_and_above_old_threshold() {
        for count in [12,13,32] {
            let mut game=GameState::new(&["A","B"],40); let player=PlayerId(0); let defender=combat::DefenderId::Player(PlayerId(1));
            let restricted=parse_card_script("Name:Must Have Friend\nTypes:Creature Human\nPT:2/2\nK:CARDNAME can't attack alone.").unwrap();
            let ordinary=parse_card_script("Name:Friend\nTypes:Creature Human\nPT:2/2").unwrap();
            let mut legal=Vec::new();
            for i in 0..count { let rules=if i==0 {&restricted} else {&ordinary}; let id=game.create_card(crate::card::Card::from_rules(rules,player)); game.move_card(id,ZoneType::Battlefield,player); legal.push((id,vec![defender])); }
            let required=vec![legal[0].0];
            assert_eq!(maximum_joint_required(&required,&legal,&[defender],&[],Some(1),&game.cards),Ok(Some(0)),"count={count}");
            assert_eq!(maximum_joint_required(&required,&legal,&[defender],&[],Some(2),&game.cards),Ok(Some(1)),"count={count}");
        }
    }
}

#[cfg(test)]
mod declined_attack_mana_regression {
    use super::*;
    use crate::agent::{ManaAbilityOption, PriorityActionSpace, TargetChoice, PassAgent};
    use crate::player::actions::PlayerAction;
    use crate::spellability::SpellAbility;
    use forge_foundation::ZoneType;
    use std::{cell::Cell, rc::Rc};

    struct Agent { attacker: CardId, land: CardId, prompts: Rc<Cell<usize>> }
    impl PlayerAgent for Agent {
        fn choose_land_or_spell(&mut self, _: PlayerId) -> Option<bool> { None }
        fn mulligan_decision(&mut self, _: PlayerId, _: &[CardId], _: u32) -> bool { true }
        fn choose_action(&mut self, _: PlayerId, _: Option<&PriorityActionSpace>, _: &mut dyn FnMut() -> PriorityActionSpace) -> PlayerAction { PlayerAction::PassPriority }
        fn choose_attackers(&mut self, _: PlayerId, _: &[CardId], _: &[combat::DefenderId]) -> Vec<(CardId, combat::DefenderId)> { vec![(self.attacker, combat::DefenderId::Player(PlayerId(1)))] }
        fn choose_blockers(&mut self, _: PlayerId, _: &[CardId], _: &[CardId], _: Option<usize>) -> Vec<(CardId, CardId)> { vec![] }
        fn choose_targets_for(&mut self, sa: &mut SpellAbility, game: &GameState, pools: &[ManaPool]) -> bool { crate::spellability::choose_targets_by_kind(self, sa, game, pools) }
        fn choose_target_player(&mut self, _: PlayerId, valid: &[PlayerId], _: Option<&SpellAbility>) -> Option<PlayerId> { valid.first().copied() }
        fn choose_target_card(&mut self, _: PlayerId, valid: &[CardId], _: Option<&SpellAbility>) -> Option<CardId> { valid.first().copied() }
        fn choose_target_any(&mut self, _: PlayerId, _: &[PlayerId], _: &[CardId], _: Option<&SpellAbility>) -> TargetChoice { TargetChoice::None }
        fn confirm_payment(&mut self, _: PlayerId, _: &str, _: &str, _: Option<CardId>, _: Option<crate::ability::api_type::ApiType>) -> bool { false }
        fn pay_combat_cost(&mut self, _: PlayerId, _: CardId, _: i32, _: &str, options: &[ManaAbilityOption], lands: &[CardId], _: &[CardId], _: i32) -> CombatCostAction {
            let count = self.prompts.get(); self.prompts.set(count + 1);
            assert!(count < 3, "declined mana ability was repeatedly offered");
            if count == 0 {
                assert!(options.iter().any(|o| o.card_id == self.land));
                CombatCostAction::TapLand { card_id: self.land, mana_ability_index: Some(0), express_choice: None }
            } else {
                assert!(!options.iter().any(|o| o.card_id == self.land));
                assert!(!lands.contains(&self.land));
                CombatCostAction::Decline
            }
        }
    }

    #[test]
    fn declined_life_mana_is_not_reoffered_for_attack_tax() {
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
        let mut game = GameState::new(&["A", "B"], 40);
        let add = |game: &mut GameState, script: &str, owner| {
            let rules = forge_carddb::parse_card_script(script).unwrap();
            let id = game.create_card(crate::card::Card::from_rules(&rules, owner));
            game.move_card(id, ZoneType::Battlefield, owner); id
        };
        let attacker = add(&mut game, "Name:Attacker\nTypes:Creature Human\nPT:2/2", PlayerId(0));
        game.card_mut(attacker).set_summoning_sick(false);
        let land = add(&mut game, "Name:Mana Confluence\nTypes:Land\nA:AB$ Mana | Cost$ T PayLife<1> | Produced$ Any", PlayerId(0));
        add(&mut game, "Name:Tax\nTypes:Enchantment\nS:Mode$ CantAttackUnless | ValidCard$ Creature | Target$ You | Cost$ 1", PlayerId(1));
        let prompts = Rc::new(Cell::new(0));
        let mut agents: Vec<Box<dyn PlayerAgent>> = vec![Box::new(Agent { attacker, land, prompts: prompts.clone() }), Box::new(PassAgent)];
        GameLoop::new(2).step_combat(&mut game, &mut agents);
        assert_eq!(prompts.get(), 2);
        assert!(!game.card(land).tapped);
        assert_eq!(game.player(PlayerId(0)).life, 40);
        assert_eq!(game.player(PlayerId(1)).life, 40);
        }).unwrap().join().unwrap();
    }
}
