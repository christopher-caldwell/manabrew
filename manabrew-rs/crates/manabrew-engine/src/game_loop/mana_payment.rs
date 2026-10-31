use super::*;

use forge_foundation::{ManaCost, ZoneType};

use crate::ability::activated::ActivatedAbility;

#[derive(Clone, Copy)]
pub(crate) struct ManaPaymentSession<'a> {
    pub player: PlayerId,
    pub card_id: CardId,
    pub card_name: &'a str,
    pub mana_cost: &'a ManaCost,
    pub cost_str: &'a str,
    pub cost_display_str: &'a str,
    pub cost_checkpoint_str: &'a str,
    pub is_activated_ability: bool,
    pub reserved_sacrifices: &'a [CardId],
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManaAbilityResolution { Resolved, Failed, FailedPreservingEffects }

#[derive(Clone, Copy)]
pub(crate) struct ManaPaymentResult {
    pub paid: bool,
    pub preserve_taps_on_failure: bool,
    pub convoke_used: bool,
    pub independent_mana_effects: bool,
}

impl ManaPaymentResult {
    fn paid() -> Self {
        Self {
            paid: true,
            preserve_taps_on_failure: false,
            convoke_used: false,
            independent_mana_effects: false,
        }
    }

    fn failed() -> Self {
        Self {
            paid: false,
            preserve_taps_on_failure: false,
            convoke_used: false,
            independent_mana_effects: false,
        }
    }

    fn failed_preserving_taps(convoke_used: bool, independent_mana_effects: bool) -> Self {
        Self {
            paid: false,
            preserve_taps_on_failure: true,
            convoke_used,
            independent_mana_effects,
        }
    }
}

fn notify_mana_payment_resolved(
    agents: &mut [Box<dyn PlayerAgent>],
    player: PlayerId,
    actions: &[ManaCostAction],
) {
    let notification = crate::agent::notification::GameNotification::ManaPaymentResolved {
        player,
        actions: actions.to_vec(),
    };
    for agent in agents.iter_mut() {
        agent.notify(notification.clone());
    }
}

pub(crate) fn pay_mana_cost_session_generic<
    FAvail,
    FAuto,
    FTryPay,
    FResolveAbility,
    FBasicLandTap,
    FUndoable,
    FBeginUndo,
    FFinishUndo,
    FUndo,
    TUndo,
>(
    game: &mut GameState,
    agents: &mut [Box<dyn PlayerAgent>],
    mana_pools: &mut [ManaPool],
    session: ManaPaymentSession<'_>,
    mana_ability_available: FAvail,
    mut auto_pay: FAuto,
    mut try_pay_from_pool: FTryPay,
    mut resolve_mana_ability: FResolveAbility,
    mut on_basic_land_tap: FBasicLandTap,
    mut undoable_mana_sources: FUndoable,
    mut begin_mana_undo: FBeginUndo,
    mut finish_mana_undo: FFinishUndo,
    mut undo_mana_action: FUndo,
) -> ManaPaymentResult
where
    FAvail: Fn(&GameState, PlayerId, CardId, &ActivatedAbility, &[CardId]) -> bool,
    FAuto: FnMut(
        &mut GameState,
        &mut [Box<dyn PlayerAgent>],
        &mut [ManaPool],
        ManaPaymentSession<'_>,
    ) -> Option<Vec<ManaCostAction>>,
    FTryPay: FnMut(&mut GameState, &mut [ManaPool], PlayerId) -> bool,
    FResolveAbility: FnMut(
        &mut GameState,
        &mut [Box<dyn PlayerAgent>],
        &mut [ManaPool],
        PlayerId,
        CardId,
        &ActivatedAbility,
        Option<u16>,
    ) -> ManaAbilityResolution,
    FBasicLandTap: FnMut(&mut GameState, PlayerId, CardId),
    FUndoable: FnMut(&GameState, &[ManaPool], PlayerId) -> Vec<CardId>,
    FBeginUndo: FnMut(&GameState, &[ManaPool], PlayerId, CardId) -> TUndo,
    FFinishUndo: FnMut(&mut GameState, &mut [ManaPool], TUndo, usize),
    FUndo: FnMut(&mut GameState, &mut [ManaPool], PlayerId, CardId) -> bool,
{
    let saved_pool = mana_pools[session.player.index()].clone();
    let mut mana_loop_invalid_count = 0u32;
    // A declined activation is not progress. Hide that ability (all colors)
    // until a different successful action changes the payment state.
    let mut rejected_mana_abilities = std::collections::HashSet::new();
    let mut executed_actions: Vec<ManaCostAction> = Vec::new();
    let mut convoke_used = false;
    let mut convoke_records: Vec<(CardId, u64)> = Vec::new();
    // Only the narrowly recognized independent mana/draw path can be
    // reversed without restoring an irreversible mana activation.
    let mut convoke_reverse_safe = true;
    let mut independent_mana_effects = true;
    let mut preserved_nested_effects = false;

    macro_rules! fail_with_undo {
        () => {{
                // Undoable mana activations may be reversed with the failed
                // cast. Preserve the prior path for any activation that the
                // engine cannot prove undoable; do not silently refund it.
                let mut outstanding = Vec::new();
                for action in &executed_actions {
                    match action {
                        ManaCostAction::TapForMana { card_id, .. } => outstanding.push(*card_id),
                        ManaCostAction::Untap(card_id) => {
                            if let Some(index) = outstanding.iter().rposition(|id| id == card_id) {
                                outstanding.remove(index);
                            }
                        }
                        _ => {}
                    }
                }
                let mut fully_undoable = !preserved_nested_effects;
                while let Some(&source) = outstanding.last() {
                    let undoable = undoable_mana_sources(game, mana_pools, session.player);
                    if !undoable.contains(&source) || !undo_mana_action(game, mana_pools, session.player, source) {
                        fully_undoable = false;
                        break;
                    }
                    outstanding.pop();
                    executed_actions.push(ManaCostAction::Untap(source));
                }
                executed_actions.push(ManaCostAction::AttemptedAndFailed);
                notify_mana_payment_resolved(agents, session.player, &executed_actions);
                if fully_undoable {
                    mana_pools[session.player.index()] = saved_pool.clone();
                    return ManaPaymentResult::failed();
                }
                // Preserve the completed mana effects but retract pending
                // convoke contributions, never an arbitrary same-color mana.
                if convoke_used {
                    if !convoke_reverse_safe { panic!("unsupported mixed convoke cancellation after complex mana effect"); }
                    for &(source, epoch) in &convoke_records {
                        if !outstanding.contains(&source) { continue; } // reversed by the snapshot stack
                        let active = mana_pools[session.player.index()].mana_entries().iter().any(|m|
                            m.convoke_for == Some(session.card_id) && m.convoke_source == Some(source));
                        if !active { panic!("missing pending convoke contribution during cancellation"); }
                        let card = game.card(source);
                        if card.zone != ZoneType::Battlefield || card.zone_timestamp != epoch || !card.tapped {
                            panic!("unsupported mixed convoke cancellation after source changed");
                        }
                        assert!(mana_pools[session.player.index()].remove_convoke(session.card_id, source));
                        // Reversal is not a game untap event and cannot be replaced.
                        game.card_mut(source).tapped = false;
                    }
                }
                return ManaPaymentResult::failed_preserving_taps(false, independent_mana_effects);
        }};
    }

    loop {
        let mana_sources =
            mana::collect_mana_payment_sources(game, session.player, session.reserved_sacrifices);
        let lands = mana_sources.source_cards.clone();
        let convoke_sources = convoke_payment_sources(game, session.player, session.card_id);
        let mut tappable_lands = lands;
        tappable_lands.extend(convoke_sources.iter().copied());
        let mut mana_ability_options = mana_sources.mana_ability_options;
        // Offer costed mana abilities when their costs can be paid without
        // using the ability's own source.
        for &source in &mana_sources.source_cards {
            for ab in &game.card(source).activated_abilities {
                let activation_ctx = mana::ManaPaymentContext { is_activated_ability:true, payment_card:Some(source), ..Default::default() };
                if !ab.is_mana_ability || !ab.cost.parts.iter().any(|p| matches!(p, crate::cost::CostPart::Mana{..}))
                    || !crate::cost::can_pay_ignoring_mana(&ab.cost,game,source,session.player)
                    || !mana::can_pay_mana_cost_with_reserved_sacrifices(game,&mana_pools[session.player.index()],session.player,source,&ab.cost,session.reserved_sacrifices,Some(&activation_ctx)) { continue; }
                let (produced_mana,produced_mana_amount)=mana::mana_ability_prompt_metadata(game,source,session.player,ab);
                mana_ability_options.push(crate::agent::ManaAbilityOption {card_id:source,ability_index:ab.ability_index,description:ab.ability_text.clone(),cost:ab.cost_string(),produced_mana,produced_mana_amount});
            }
        }
        mana_ability_options.retain(|option| !rejected_mana_abilities.contains(&(option.card_id, option.ability_index)));
        let untappable_lands = undoable_mana_sources(game, mana_pools, session.player);
        let pool_ref = mana_pools[session.player.index()].clone();
        let can_confirm_from_pool = {
            let mut confirm_pool = pool_ref.clone();
            confirm_pool.source_colors = None;
            confirm_pool.total_sources = None;
            // `try_pay` treats an unpaid Phyrexian shard as if life can
            // always cover it. This prompt promises a pool-only payment, so
            // require the colored alternative instead of assuming life.
            confirm_pool.try_pay(&session.mana_cost.phyrexian_to_colored())
        };

        agents[session.player.index()].snapshot_state(game, mana_pools);
        let action = agents[session.player.index()].pay_mana_cost(
            session.player,
            session.card_id,
            session.card_name,
            session.cost_str,
            session.cost_display_str,
            session.cost_checkpoint_str,
            can_confirm_from_pool,
            session.is_activated_ability,
            session.reserved_sacrifices,
            &mana_ability_options,
            &tappable_lands,
            &untappable_lands,
            &pool_ref,
        );

        match action {
            ManaCostAction::TapForMana {
                card_id: land_id,
                mana_ability_index,
                express_choice,
            } => {
                if mana_ability_index == Some(usize::MAX) && convoke_sources.contains(&land_id) {
                    let player_idx = session.player.index();
                    let atom = express_choice
                        .filter(|&a| a != 0)
                        .unwrap_or_else(|| convoke_atom(game, land_id, session.mana_cost));
                    let undo_record = begin_mana_undo(game, mana_pools, session.player, land_id);
                    if !game.tap(land_id) {
                        mana_loop_invalid_count += 1;
                        if mana_loop_invalid_count > 3 { fail_with_undo!(); }
                        continue;
                    }
                    convoke_used = true;
                    mana_loop_invalid_count = 0;
                    rejected_mana_abilities.clear();
                    let mut contribution = mana::Mana::simple(atom);
                    contribution.convoke_for = Some(session.card_id);
                    contribution.convoke_source = Some(land_id);
                    mana_pools[player_idx].add_convoke_contribution(contribution);
                    convoke_records.push((land_id, game.card(land_id).zone_timestamp));
                    finish_mana_undo(game, mana_pools, undo_record, 1);
                    executed_actions.push(ManaCostAction::TapForMana {
                        card_id: land_id,
                        mana_ability_index: None,
                        express_choice: Some(atom),
                    });
                    continue;
                }
                if !tappable_lands.contains(&land_id)
                    || mana_ability_index.is_some_and(|idx| rejected_mana_abilities.contains(&(land_id, idx))) {
                    mana_loop_invalid_count += 1;
                    if mana_loop_invalid_count > 3 { fail_with_undo!(); }
                    continue;
                }
                let mana_ab = {
                    let c = game.card(land_id);
                    match mana_ability_index {
                        Some(idx) => c.activated_abilities.iter().find(|ab| ab.ability_index == idx),
                        None => c.activated_abilities.iter().find(|ab| ab.is_mana_ability && mana_ability_available(game, session.player, land_id, ab, session.reserved_sacrifices)),
                    }.filter(|ab| ab.is_mana_ability && mana_ability_available(game, session.player, land_id, ab, session.reserved_sacrifices)).cloned()
                };
                if let Some(ab) = mana_ab {
                    // Snapshot BEFORE the ability produces mana so we can
                    // capture everything this tap adds to the pool —
                    // base production, aura-granted mana, doublers, and
                    // TapsForMana trigger payloads — in a single diff.
                    // Without this, the untap path below only knows about
                    // the land's native atoms and leaves the aura-added
                    // mana orphaned in the pool.
                    let player_idx = session.player.index();
                    let undo_record = begin_mana_undo(game, mana_pools, session.player, land_id);
                    let old_tapped = game.card(land_id).tapped;
                    let old_timestamp = game.card(land_id).zone_timestamp;
                    let old_life = game.player(session.player).life;
                    let old_counters = game.card(land_id).counters.clone();
                    let pool_snapshot = mana_pools[player_idx].begin_tap_tracking();
                    let ability_independent = independent_nested_mana_effects(game, session.player, land_id, &ab);
                    let independent = convoke_records.is_empty() ||
                        (independent_mana_draw_only(game, land_id, &ab) && !convoke_records.iter().any(|(source, _)| *source == land_id));
                    // A convoke tap pays this spell, never the activation cost
                    // of a mana ability used while casting it.
                    let held = mana_pools[player_idx].take_convoke_contributions(session.card_id);
                    let resolution = resolve_mana_ability(
                        game, agents, mana_pools, session.player, land_id, &ab, express_choice,
                    );
                    mana_pools[player_idx].restore_convoke_contributions(held);
                    let resolved = resolution == ManaAbilityResolution::Resolved;
                    if resolution == ManaAbilityResolution::FailedPreservingEffects {
                        preserved_nested_effects = true;
                        // The nested activation is not the pending ability;
                        // its effects remain even when the outer cast fails.
                        convoke_reverse_safe = false;
                    }
                    if resolved { convoke_reverse_safe &= independent; independent_mana_effects &= ability_independent; }
                    let produced = mana_pools[player_idx].end_tap_tracking(&pool_snapshot);
                    let produced_count = produced.len();
                    let progressed = resolution == ManaAbilityResolution::FailedPreservingEffects
                        || (resolved && (produced_count > 0
                        || game.card(land_id).tapped != old_tapped
                        || game.card(land_id).zone_timestamp != old_timestamp
                        || game.card(land_id).counters != old_counters
                        || game.player(session.player).life != old_life));
                    if resolved {
                        executed_actions.push(ManaCostAction::TapForMana {
                            card_id: land_id,
                            mana_ability_index: Some(mana_ability_index.unwrap_or(0)),
                            express_choice,
                        });
                    }
                    if progressed {
                        mana_loop_invalid_count = 0;
                        rejected_mana_abilities.clear();
                    } else {
                        rejected_mana_abilities.insert((land_id, ab.ability_index));
                        mana_loop_invalid_count += 1;
                    }
                    if resolved && !produced.is_empty() {
                        game.card_mut(land_id).last_mana_produced = Some(produced);
                    }
                    finish_mana_undo(game, mana_pools, undo_record, produced_count);
                } else if let Some(atom) = mana_ability_index.is_none().then(|| basic_land_mana_atom(game.card(land_id))).flatten() {
                    mana_loop_invalid_count = 0;
                    rejected_mana_abilities.clear();
                    executed_actions.push(ManaCostAction::TapForMana {
                        card_id: land_id,
                        mana_ability_index: Some(0),
                        express_choice: None,
                    });
                    let player_idx = session.player.index();
                    let undo_record = begin_mana_undo(game, mana_pools, session.player, land_id);
                    let pool_snapshot = mana_pools[player_idx].begin_tap_tracking();
                    let basic_independent = no_active_trigger_or_replacement(game);
                    independent_mana_effects &= basic_independent;
                    if !convoke_records.is_empty() {
                        convoke_reverse_safe &= basic_independent
                            && !convoke_records.iter().any(|(source, _)| *source == land_id);
                    }
                    game.tap(land_id);
                    // Fire ProduceMana replacement (e.g. Nyxbloom Ancient triples mana)
                    // before adding to pool. Mirrors Java AbilityManaPart.produceMana
                    // which always invokes ReplacementHandler.run(ProduceMana, ...)
                    // even for the implicit basic-land tap (every land has an
                    // intrinsic AbilityManaPart in Forge's CardFactoryUtil).
                    let mana_letter = crate::mana::ManaPool::atom_to_letter(atom).to_string();
                    let mut event =
                        crate::replacement::replacement_handler::ReplacementEvent::ProduceMana {
                            source: land_id,
                            activator: session.player,
                            mana: mana_letter.clone(),
                        };
                    let result =
                        crate::replacement::replacement_handler::apply_replacements_with_agents(
                            game, agents, &mut event,
                        );
                    let final_mana = if result == crate::replacement::ReplacementResult::Updated {
                        if let crate::replacement::replacement_handler::ReplacementEvent::ProduceMana {
                            mana,
                            ..
                        } = event
                        {
                            mana
                        } else {
                            mana_letter
                        }
                    } else {
                        mana_letter
                    };
                    for token in final_mana.split_whitespace() {
                        if let Some(produced_atom) = crate::mana::mana_atom_from_produced(token) {
                            mana_pools[player_idx].add(produced_atom, 1);
                        }
                    }
                    on_basic_land_tap(game, session.player, land_id);
                    let produced = mana_pools[player_idx].end_tap_tracking(&pool_snapshot);
                    let produced_count = produced.len();
                    if !produced.is_empty() {
                        game.card_mut(land_id).last_mana_produced = Some(produced);
                    }
                    finish_mana_undo(game, mana_pools, undo_record, produced_count);
                } else {
                    if let Some(idx) = mana_ability_index { rejected_mana_abilities.insert((land_id, idx)); }
                    mana_loop_invalid_count += 1;
                }
                if mana_loop_invalid_count > 3 { fail_with_undo!(); }
            }
            ManaCostAction::Untap(land_id) => {
                if !untappable_lands.contains(&land_id) {
                    continue;
                }
                if undo_mana_action(game, mana_pools, session.player, land_id) {
                    if let Some(index) = convoke_records.iter().rposition(|(source, _)| *source == land_id) {
                        convoke_records.remove(index);
                    }
                    mana_loop_invalid_count = 0;
                    rejected_mana_abilities.clear();
                    executed_actions.push(ManaCostAction::Untap(land_id));
                }
            }
            ManaCostAction::Pay { auto } => {
                if auto {
                    if let Some(mut auto_trace) = auto_pay(game, agents, mana_pools, session) {
                        let attempted_and_failed =
                            matches!(auto_trace.last(), Some(ManaCostAction::AttemptedAndFailed));
                        executed_actions.append(&mut auto_trace);
                        if attempted_and_failed {
                            notify_mana_payment_resolved(agents, session.player, &executed_actions);
                            // The auto-payer refunded consumed mana objects and
                            // retained its activations; do not restore a stale pool.
                            return ManaPaymentResult::failed_preserving_taps(convoke_used, false);
                        }
                        if mana_pools[session.player.index()].mana_entries().iter().any(|m| m.convoke_for == Some(session.card_id)) {
                            panic!("unsupported unused convoke contribution after auto payment");
                        }
                        executed_actions.push(ManaCostAction::Pay { auto: false });
                        notify_mana_payment_resolved(agents, session.player, &executed_actions);
                        return ManaPaymentResult::paid();
                    }
                    // A failed automatic plan can follow manual activations.
                    // Undo those actions or preserve their irreversible effects,
                    // exactly as for an explicit cancellation.
                    fail_with_undo!();
                }

                if try_pay_from_pool(game, mana_pools, session.player) {
                    if mana_pools[session.player.index()].mana_entries().iter().any(|m| m.convoke_for == Some(session.card_id)) {
                        panic!("unsupported unused convoke contribution after payment");
                    }
                    executed_actions.push(ManaCostAction::Pay { auto: false });
                    notify_mana_payment_resolved(agents, session.player, &executed_actions);
                    return ManaPaymentResult::paid();
                }

                mana_loop_invalid_count += 1;
                if mana_loop_invalid_count > 3 {
                    fail_with_undo!();
                }
            }
            ManaCostAction::AttemptedAndFailed => { fail_with_undo!(); }
        }
    }
}

impl GameLoop {
    pub(crate) fn make_mana_payment_callback<'a>(
        trigger_handler: *mut TriggerHandler,
        game: *mut GameState,
        agents: &'a mut [Box<dyn PlayerAgent>],
        player: PlayerId,
        source: CardId,
    ) -> impl FnMut(mana::ManaPayCallback<'_>) -> Option<CardId> + 'a {
        move |kind: mana::ManaPayCallback<'_>| -> Option<CardId> {
            match kind {
                mana::ManaPayCallback::ChooseSacrifice(valid) => {
                    agents[player.index()].choose_sacrifice(player, valid, Some(source))
                }
                mana::ManaPayCallback::ChooseColor(valid_colors) => {
                    // Always invoke the agent — humans go through their
                    // interactive `ChooseColor` modal, AI returns a
                    // default. The engine never branches on agent kind.
                    let _ = agents[player.index()].choose_color(player, valid_colors);
                    None
                }
                mana::ManaPayCallback::ChooseTapType {
                    valid,
                    min,
                    max,
                    chosen,
                } => {
                    chosen.extend(
                        agents[player.index()].choose_cards_for_effect(player, valid, min, max),
                    );
                    chosen.first().copied()
                }
                mana::ManaPayCallback::ConfirmSelfSacrifice(source_id) => {
                    if agents[player.index()].confirm_payment(
                        player,
                        "Sacrifice",
                        "Sacrifice for mana",
                        Some(source_id),
                        Some(crate::ability::api_type::ApiType::Mana),
                    ) {
                        Some(source_id)
                    } else {
                        None
                    }
                }
                mana::ManaPayCallback::ConfirmSubCounter(source_id) => {
                    if agents[player.index()].confirm_payment(
                        player,
                        "SubCounter",
                        "Remove counter for mana",
                        Some(source_id),
                        Some(crate::ability::api_type::ApiType::Mana),
                    ) {
                        Some(source_id)
                    } else {
                        None
                    }
                }
                mana::ManaPayCallback::ConfirmSourceExile(source_id) => {
                    if agents[player.index()].confirm_payment(
                        player,
                        "Exile",
                        "Exile for mana",
                        Some(source_id),
                        Some(crate::ability::api_type::ApiType::Mana),
                    ) {
                        Some(source_id)
                    } else {
                        None
                    }
                }
                mana::ManaPayCallback::ConfirmPayLife(source_id) => {
                    if agents[player.index()].confirm_payment(
                        player,
                        "PayLife",
                        "Pay life for mana",
                        Some(source_id),
                        Some(crate::ability::api_type::ApiType::Mana),
                    ) {
                        Some(source_id)
                    } else {
                        None
                    }
                }
                mana::ManaPayCallback::NotifySacrificeForMana(sacrificed_id) => unsafe {
                    let game = &mut *game;
                    let trigger_handler = &mut *trigger_handler;
                    let owner = game.card(sacrificed_id).owner;
                    let controller = game.card(sacrificed_id).controller;
                    let lki_counters = game.card(sacrificed_id).counters.clone();
                    let lki_power = game.card(sacrificed_id).power();
                    let lki_toughness = game.card(sacrificed_id).toughness();
                    let lki_p1p1 = *lki_counters
                        .get(&crate::card::CounterType::P1P1)
                        .unwrap_or(&0);
                    {
                        let card = game.card_mut(sacrificed_id);
                        card.lki_counters = Some(lki_counters);
                        card.set_lki_power_toughness(Some(lki_power), Some(lki_toughness));
                    }
                    game.last_sacrificed_card = Some(sacrificed_id);
                    trigger_handler.run_trigger(
                        TriggerType::Sacrificed,
                        RunParams {
                            card: Some(sacrificed_id),
                            player: Some(controller),
                            ..Default::default()
                        },
                        false,
                    );
                    crate::ability::effects::emit_zone_trigger_with_lki_counters(
                        trigger_handler, game,
                        sacrificed_id,
                        ZoneType::Battlefield,
                        ZoneType::Graveyard,
                        lki_p1p1,
                        lki_power,
                        lki_toughness,
                    );
                    trigger_handler.flush_waiting_triggers(game);
                    game.move_card(sacrificed_id, ZoneType::Graveyard, owner);
                    let mut by_controller = std::collections::BTreeMap::new();
                    by_controller.insert(controller, vec![sacrificed_id]);
                    crate::game_loop::fire_sacrificed_once_for_batch(
                        game,
                        trigger_handler,
                        &by_controller,
                    );
                    Some(sacrificed_id)
                },
                mana::ManaPayCallback::ApplyProduceManaReplacement {
                    activator,
                    source_card,
                    mana,
                } => unsafe {
                    let game = &mut *game;
                    let mut event =
                        crate::replacement::replacement_handler::ReplacementEvent::ProduceMana {
                            source: source_card,
                            activator,
                            mana: mana.clone(),
                        };
                    let result =
                        crate::replacement::replacement_handler::apply_replacements_with_agents(
                            game, agents, &mut event,
                        );
                    if result == crate::replacement::ReplacementResult::Updated {
                        if let crate::replacement::replacement_handler::ReplacementEvent::ProduceMana {
                            mana: new_mana,
                            ..
                        } = event
                        {
                            *mana = new_mana;
                        }
                    }
                    None
                },
            }
        }
    }

    pub(crate) fn pay_mana_cost_session<FAvail, FAuto, FTryPay>(
        &mut self,
        game: &mut GameState,
        agents: &mut [Box<dyn PlayerAgent>],
        session: ManaPaymentSession<'_>,
        mana_ability_available: FAvail,
        mut auto_pay: FAuto,
        mut try_pay_from_pool: FTryPay,
    ) -> ManaPaymentResult
    where
        FAvail: Fn(&GameState, PlayerId, CardId, &ActivatedAbility, &[CardId]) -> bool,
        FAuto: FnMut(
            &mut GameLoop,
            &mut GameState,
            &mut [Box<dyn PlayerAgent>],
            ManaPaymentSession<'_>,
        ) -> Option<Vec<ManaCostAction>>,
        FTryPay: FnMut(&mut GameLoop, &mut GameState, PlayerId) -> bool,
    {
        let self_ptr: *mut GameLoop = self;
        let agents_ptr: *mut [Box<dyn PlayerAgent>] = std::ptr::from_mut(agents);
        let paid = pay_mana_cost_session_generic(
            game,
            agents,
            &mut self.mana_pools,
            session,
            mana_ability_available,
            |game, agents, _mana_pools, session| unsafe {
                auto_pay(&mut *self_ptr, game, agents, session)
            },
            |game, _mana_pools, player| unsafe { try_pay_from_pool(&mut *self_ptr, game, player) },
            |game, agents, _mana_pools, player, card_id, ab, express_choice| unsafe {
                (&mut *self_ptr).resolve_mana_ability_with_outcome(
                    game,
                    agents,
                    player,
                    card_id,
                    ab,
                    express_choice,
                )
            },
            |game, player, land_id| unsafe {
                let this = &mut *self_ptr;
                let agents = &mut *agents_ptr;
                this.trigger_handler.run_trigger(
                    TriggerType::TapsForMana,
                    RunParams {
                        card: Some(land_id),
                        player: Some(player),
                        ..Default::default()
                    },
                    false,
                );
                this.trigger_handler.run_trigger(
                    TriggerType::ManaAdded,
                    RunParams {
                        card: Some(land_id),
                        player: Some(player),
                        activator: Some(player),
                        ..Default::default()
                    },
                    false,
                );
                let pending = this.trigger_handler.run_waiting_triggers(game);
                if !pending.is_empty() {
                    this.mark_mana_undo_disqualified();
                }
                for pt in pending {
                    this.resolve_single_effect(game, agents, &pt.entry.spell_ability, None);
                }
            },
            |_game, _mana_pools, player| unsafe { (&*self_ptr).undoable_mana_sources(player) },
            |game, mana_pools, player, card_id| unsafe {
                (&mut *self_ptr)
                    .begin_mana_undo_action_with_mana_slice(game, mana_pools, player, card_id)
            },
            |_game, _mana_pools, record, produced_count| unsafe {
                (&mut *self_ptr).finish_mana_undo_action(record, produced_count);
            },
            |game, mana_pools, player, card_id| unsafe {
                (&mut *self_ptr).undo_mana_action_with_mana_slice(game, mana_pools, player, card_id)
            },
        );
        self.invalidate_mana_undo_for_player(session.player);
        paid
    }
}

// Deliberately narrow proof for selective reversal: ordinary tap mana, with
// at most a direct draw for its controller, and no active global triggered or
// replacement effects. Other cases keep the existing unsupported guard.
fn no_active_trigger_or_replacement(game: &GameState) -> bool {
    !game.cards.iter().any(|c| matches!(c.zone, ZoneType::Battlefield | ZoneType::Command)
        && (!c.triggers.is_empty() || !c.replacement_effects.is_empty()))
}

fn independent_nested_mana_effects(game: &GameState, player: PlayerId, source: CardId, ab: &ActivatedAbility) -> bool {
    if independent_mana_draw_only(game, source, ab) { return true; }
    if !no_active_trigger_or_replacement(game) || !ab.ability_kind.eq_ignore_ascii_case("Mana")
        || ab.sub_ability.is_some() || ab.cost.parts.len() != 2 { return false; }
    let tap = ab.cost.parts.iter().filter(|p| matches!(p, crate::cost::CostPart::Tap)).count() == 1;
    let self_sac = ab.cost.parts.iter().filter(|p| matches!(p, crate::cost::CostPart::Sacrifice {type_filter, amount}
        if type_filter == "CARDNAME" && amount.resolve(game, source, player) == 1)).count() == 1;
    tap && self_sac
}

fn independent_mana_draw_only(game: &GameState, source: CardId, ab: &ActivatedAbility) -> bool {
    if !ab.ability_kind.eq_ignore_ascii_case("Mana") || ab.cost.parts.len() != 1
        || !matches!(ab.cost.parts[0], crate::cost::CostPart::Tap) { return false; }
    if !no_active_trigger_or_replacement(game) { return false; }
    match &ab.sub_ability {
        None => true,
        Some(key) => game.card(source).svars.get(key).is_some_and(|raw| {
            let p = crate::parsing::Params::from_raw(raw);
            p.get("DB") == Some("Draw") && p.get("Defined") == Some("You")
                && p.get("NumCards").is_some_and(|n| n.parse::<u32>().is_ok())
                && p.inner().keys().all(|k| matches!(k.as_str(), "DB" | "Defined" | "NumCards" | "SpellDescription"))
        }),
    }
}

fn convoke_payment_sources(game: &GameState, player: PlayerId, spell: CardId) -> Vec<CardId> {
    let card = game.card(spell);
    let has_convoke = card.has_keyword("Convoke");
    let has_improvise = card.has_keyword("Improvise");
    if !has_convoke && !has_improvise {
        return Vec::new();
    }
    game.cards_in_zone(ZoneType::Battlefield, player)
        .iter()
        .filter(|&&cid| {
            if cid == spell {
                return false;
            }
            let c = game.card(cid);
            !c.tapped
                && ((has_convoke && c.is_creature())
                    || (has_improvise && c.type_line.is_artifact()))
        })
        .copied()
        .collect()
}

fn convoke_atom(game: &GameState, source: CardId, cost: &ManaCost) -> u16 {
    let card = game.card(source);
    if !card.is_creature() {
        return forge_foundation::ManaAtom::COLORLESS;
    }
    let colors = card.color.mask() as u16 & forge_foundation::ManaAtom::ALL_MANA_COLORS;
    if colors == 0 {
        return forge_foundation::ManaAtom::COLORLESS;
    }
    let needed = colors & (cost.color_profile() as u16);
    let pick = if needed != 0 { needed } else { colors };
    1u16 << pick.trailing_zeros()
}
