use forge_foundation::ZoneType;

use super::{emit_zone_trigger, resolve_numeric_svar, EffectContext};
use crate::ids::CardId;
use crate::parsing::keys;

/// `SP$ TwoPiles` — an opponent divides cards into two piles and the controller chooses one.
///
/// Mirrors Java's `TwoPilesEffect.java`: an opponent separates, the controller chooses.
/// - `NumCards$` — number of cards to reveal from library (default 5).
/// - `Zone1$` — destination for the chosen pile (default Hand).
/// - `Zone2$` — destination for the unchosen pile (default Graveyard).
///
/// # Card script examples
/// ```text
/// A:SP$ TwoPiles | NumCards$ 5 | Zone1$ Hand | Zone2$ Graveyard
/// A:SP$ TwoPiles | NumCards$ 3 | Zone1$ Hand | Zone2$ Library
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `TwoPilesEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(TwoPilesEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    let controller = sa.activating_player;
    let num_cards = resolve_numeric_svar(ctx.game, sa, keys::NUM_CARDS, 5).max(0) as usize;

    let zone1 = sa.ir.zone1.unwrap_or(ZoneType::Hand);
    let zone2 = sa.ir.zone2.unwrap_or(ZoneType::Graveyard);

    // Get top N cards from library
    let lib = ctx
        .game
        .cards_in_zone(ZoneType::Library, controller)
        .to_vec();
    if lib.is_empty() {
        return;
    }

    let count = num_cards.min(lib.len());
    // Library is bottom-to-top, so take from the end
    let revealed: Vec<CardId> = lib[lib.len() - count..].to_vec();

    if revealed.is_empty() {
        return;
    }

    // An opponent separates the revealed cards into two piles and the controller
    // chooses one. The separator is offered every revealed card, so it can weigh them.
    let opponent = ctx.game.opponent_of(controller);
    let min_pile1 = if count > 1 { 1 } else { 0 };
    let max_pile1 = if count > 1 { count - 1 } else { count };

    let pile1: Vec<CardId> = ctx.agents[opponent.index()]
        .choose_cards_for_zone_change(
            ctx.game,
            opponent,
            &revealed,
            min_pile1,
            max_pile1,
            "Separate the cards into two piles: choose the cards for pile 1",
        )
        .into_iter()
        .filter(|c| revealed.contains(c))
        .collect();
    let pile2: Vec<CardId> = revealed
        .iter()
        .filter(|c| !pile1.contains(c))
        .copied()
        .collect();

    // The controller names one card of the pile to take. The offered cards list pile 1
    // first, then pile 2; the prompt says how many belong to pile 1.
    let offered: Vec<CardId> = pile1.iter().chain(pile2.iter()).copied().collect();
    let prompt = format!(
        "Choose a pile: the first {} cards are Pile 1 and the rest are Pile 2. Select one card from the pile you take",
        pile1.len(),
    );
    let pick = ctx.agents[controller.index()]
        .choose_cards_for_zone_change(ctx.game, controller, &offered, 1, 1, &prompt)
        .into_iter()
        .next();
    let choose_pile1 = match pick {
        Some(card) => pile1.contains(&card),
        None => pile2.is_empty(),
    };

    let (chosen_pile, unchosen_pile) = if choose_pile1 {
        (pile1, pile2)
    } else {
        (pile2, pile1)
    };

    // Move chosen pile to zone1
    for cid in chosen_pile {
        if ctx.game.card(cid).zone == ZoneType::Library {
            ctx.move_card(cid, zone1, controller);
            emit_zone_trigger(ctx.trigger_handler, ctx.game, cid, ZoneType::Library, zone1);
        }
    }

    // Move unchosen pile to zone2
    for cid in unchosen_pile {
        if ctx.game.card(cid).zone == ZoneType::Library {
            ctx.move_card(cid, zone2, controller);
            emit_zone_trigger(ctx.trigger_handler, ctx.game, cid, ZoneType::Library, zone2);
        }
    }
}
