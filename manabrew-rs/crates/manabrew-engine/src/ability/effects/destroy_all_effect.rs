use forge_foundation::ZoneType;

use super::{matches_valid_cards_for_sa, EffectContext};
use crate::event::RunParams;
use crate::ids::CardId;
use crate::replacement::replacement_handler::{apply_replacements, ReplacementEvent};
use crate::replacement::ReplacementResult;
use crate::trigger::TriggerType;

/// `SP$ DestroyAll` — destroy all permanents matching `ValidCards$`.
///
/// Mirrors Java's `DestroyAllEffect.java`:
/// - Collects all matching battlefield cards (two-pass to avoid borrow issues).
/// - Respects `Indestructible` (keyword or R$-based replacement effect).
/// - `NoRegen$ True` is noted but regeneration is not yet implemented, so it
///   has no runtime effect.
///
/// # Card script examples
/// ```text
/// A:SP$ DestroyAll | ValidCards$ Creature | NoRegen$ True
/// A:SP$ DestroyAll | ValidCards$ Permanent.nonArtifact
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `DestroyAllEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(DestroyAllEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let valid_cards = sa.ir.valid_cards_selector.as_ref();

    // Pass 1 — collect matching battlefield cards
    let player_ids = ctx.game.player_order.clone();
    let mut to_destroy: Vec<CardId> = Vec::new();
    for &pid in &player_ids {
        let zone_cards = ctx.game.cards_in_zone(ZoneType::Battlefield, pid).to_vec();
        for cid in zone_cards {
            if matches_valid_cards_for_sa(ctx.game, sa, ctx.game.card(cid), valid_cards, "Creature")
            {
                to_destroy.push(cid);
            }
        }
    }

    // Capture the shared pre-departure state. Movement replacements are
    // applied to the actual game before matching events, so an exile replacement
    // never creates a fictional death trigger.
    if to_destroy.is_empty() { return; }
    let before = ctx.game.boxed_snapshot();
    let mut departed = Vec::new();
    for card_id in to_destroy {
        if ctx.game.card(card_id).zone != ZoneType::Battlefield || ctx.game.card(card_id).has_keyword("Indestructible") { continue; }
        let mut event = ReplacementEvent::Destroy { target: card_id };
        if apply_replacements(ctx.game, &mut event) == ReplacementResult::Replaced { continue; }
        let card = before.card(card_id);
        let (owner, counters, power, toughness) = (card.owner,card.counters.clone(),card.power(),card.toughness());
        let p1p1 = card.counter_count(&crate::card::CounterType::P1P1);
        ctx.game.card_mut(card_id).lki_counters = Some(counters);
        ctx.game.card_mut(card_id).set_lki_power_toughness(Some(power),Some(toughness));
        let attached=card.attached_to.map(|id|(id,before.card(id).zone_timestamp));
        let destination = ctx.move_card_destination(card_id,ZoneType::Graveyard,owner);
        if destination != ZoneType::Battlefield { departed.push((card_id,destination,p1p1,power,toughness,attached)); }
    }
    let mut events = Vec::new();
    for (id,destination,p1p1,power,toughness,attached) in departed {
        events.push((TriggerType::Destroyed,RunParams{card:Some(id),causer:sa.source,cause_card:sa.source,cause_player:Some(sa.activating_player),..Default::default()}));
        events.push((TriggerType::ChangesZone,RunParams{card:Some(id),card_lki:Some(id),origin:Some(ZoneType::Battlefield),destination:Some(destination),lki_p1p1_counters:Some(p1p1),lki_power:Some(power),lki_toughness:Some(toughness),lki_attachment_captured:true,lki_attached_to:attached,..Default::default()}));
    }
    ctx.trigger_handler.match_departure_batch(&before,ctx.game,events);

}

#[cfg(test)]
mod tests {
    use crate::ability::spell_ability_effect::SpellAbilityEffect;
    use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
    use crate::det::HashMap;

    use crate::ability::effects::EffectContext;
    use crate::agent::PassAgent;
    use crate::card::Card;
    use crate::game::GameState;
    use crate::ids::{CardId, PlayerId};
    use crate::mana::ManaPool;
    use crate::spellability::SpellAbility;
    use crate::trigger::handler::TriggerHandler;

    fn make_creature(game: &mut GameState, owner: PlayerId, keywords: Vec<String>) -> CardId {
        let c = Card::new(
            CardId(0),
            "Bear".into(),
            owner,
            CardTypeLine::parse("Creature - Bear"),
            ManaCost::parse("1 G"),
            ColorSet::GREEN,
            Some(2),
            Some(2),
            keywords,
            vec![],
        );
        game.create_card(c)
    }

    fn make_ctx<'a>(
        game: &'a mut GameState,
        agents: &'a mut Vec<Box<dyn crate::agent::PlayerAgent>>,
        trigger_handler: &'a mut TriggerHandler,
        mana_pools: &'a mut Vec<ManaPool>,
        token_templates: &'a HashMap<String, Card>,
        token_art_variants: &'a HashMap<(String, String), usize>,
        token_fallback: &'a HashMap<String, String>,
        edition_dates: &'a HashMap<String, String>,
        rng: &'a mut dyn crate::game_rng::GameRng,
    ) -> EffectContext<'a> {
        EffectContext {
            game,
            combat: None,
            agents,
            trigger_handler,
            token_templates,
            token_art_variants,
            token_fallback,
            edition_dates,
            mana_pools,
            parent_target_card: None,
            rng,
        }
    }

    #[test]
    fn destroy_all_wipes_creatures() {
        let mut game = GameState::new(&["Alice", "Bob"], 20);
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        let c1 = make_creature(&mut game, p0, vec![]);
        let c2 = make_creature(&mut game, p1, vec![]);
        game.move_card(c1, ZoneType::Battlefield, p0);
        game.move_card(c2, ZoneType::Battlefield, p1);

        let sa = SpellAbility::new_simple(
            None,
            p0,
            "A:SP$ DestroyAll | ValidCards$ Creature | NoRegen$ True",
        );
        let mut th = TriggerHandler::new();
        let mut agents: Vec<Box<dyn crate::agent::PlayerAgent>> =
            vec![Box::new(PassAgent), Box::new(PassAgent)];
        let mut mp = vec![ManaPool::default(), ManaPool::default()];
        let templates = HashMap::default();
        let templates_variants: HashMap<(String, String), usize> = HashMap::default();
        let token_fallback: HashMap<String, String> = HashMap::default();
        let edition_dates: HashMap<String, String> = HashMap::default();
        let mut rng_adapter = crate::game_rng::ThreadRngAdapter;
        let mut ctx = make_ctx(
            &mut game,
            &mut agents,
            &mut th,
            &mut mp,
            &templates,
            &templates_variants,
            &token_fallback,
            &edition_dates,
            &mut rng_adapter,
        );
        super::DestroyAllEffect::resolve(&mut ctx, &sa);

        assert_eq!(ctx.game.cards_in_zone(ZoneType::Battlefield, p0).len(), 0);
        assert_eq!(ctx.game.cards_in_zone(ZoneType::Battlefield, p1).len(), 0);
        assert_eq!(ctx.game.cards_in_zone(ZoneType::Graveyard, p0).len(), 1);
        assert_eq!(ctx.game.cards_in_zone(ZoneType::Graveyard, p1).len(), 1);
    }

    #[test]
    fn destroy_all_indestructible_survives() {
        let mut game = GameState::new(&["Alice", "Bob"], 20);
        let p0 = PlayerId(0);
        let mortal = make_creature(&mut game, p0, vec![]);
        let immortal = make_creature(&mut game, p0, vec!["Indestructible".to_string()]);
        game.move_card(mortal, ZoneType::Battlefield, p0);
        game.move_card(immortal, ZoneType::Battlefield, p0);

        let sa = SpellAbility::new_simple(
            None,
            p0,
            "A:SP$ DestroyAll | ValidCards$ Creature | NoRegen$ True",
        );
        let mut th = TriggerHandler::new();
        let mut agents: Vec<Box<dyn crate::agent::PlayerAgent>> =
            vec![Box::new(PassAgent), Box::new(PassAgent)];
        let mut mp = vec![ManaPool::default(), ManaPool::default()];
        let templates = HashMap::default();
        let templates_variants: HashMap<(String, String), usize> = HashMap::default();
        let token_fallback: HashMap<String, String> = HashMap::default();
        let edition_dates: HashMap<String, String> = HashMap::default();
        let mut rng_adapter = crate::game_rng::ThreadRngAdapter;
        let mut ctx = make_ctx(
            &mut game,
            &mut agents,
            &mut th,
            &mut mp,
            &templates,
            &templates_variants,
            &token_fallback,
            &edition_dates,
            &mut rng_adapter,
        );
        super::DestroyAllEffect::resolve(&mut ctx, &sa);

        // One creature destroyed, indestructible one stays
        assert_eq!(ctx.game.cards_in_zone(ZoneType::Battlefield, p0).len(), 1);
        assert_eq!(ctx.game.cards_in_zone(ZoneType::Graveyard, p0).len(), 1);
        assert!(ctx.game.card(immortal).has_keyword("Indestructible"));
    }
}
