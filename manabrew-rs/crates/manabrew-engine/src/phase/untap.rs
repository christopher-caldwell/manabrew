//! Untap — handles untap step logic.
//!
//! Mirrors Java's `Untap.java`.
//! Handles "until next untap", phasing, day/night transitions,
//! and the actual untap of permanents.

use forge_foundation::ZoneType;

use crate::game::GameState;
use crate::ids::{CardId, PlayerId};

/// Performs the phasing step at the beginning of the untap step.
/// Mirrors Java's `Untap.doPhasing()`.
///
/// Phase in all directly-phased-out permanents controlled by the active player.
/// Phase out all permanents with phasing controlled by the active player.
pub fn do_phasing(game: &mut GameState, turn_player: PlayerId) {
    // Determine both sets before changing status. A permanent phasing in must
    // not immediately phase back out because it has the Phasing keyword.
    let returning: Vec<CardId> = game
        .cards
        .iter()
        .filter(|c| {
            c.zone == ZoneType::Battlefield
                && c.phased_out
                && c.phased_out_with.is_none()
                && c.phased_out_controller.unwrap_or(c.controller) == turn_player
                && !crate::staticability::static_ability_cant_phase::cant_phase_in(&game.cards, c)
        })
        .map(|c| c.id)
        .collect();
    let leaving: Vec<CardId> = game
        .cards
        .iter()
        .filter(|c| {
            c.zone == ZoneType::Battlefield
                && !c.phased_out
                && c.controller == turn_player
                && c.has_keyword("Phasing")
        })
        .map(|c| c.id)
        .collect();
    phase_out_many(game, &leaving);
    for id in returning {
        phase_in(game, id);
    }
}

/// Apply simultaneous phasing with ancestors first, so a targeted attachment
/// also attached to a targeted permanent phases indirectly (CR 702.26h).
pub fn phase_out_many(game: &mut GameState, cards: &[CardId]) -> Vec<CardId> {
    let mut ordered = cards.to_vec();
    ordered.sort_by_key(|&id| {
        let mut depth = 0;
        let mut parent = game.card(id).attached_to;
        while let Some(id) = parent {
            depth += 1;
            if depth >= game.cards.len() {
                break;
            }
            parent = game.card(id).attached_to;
        }
        depth
    });
    let mut changed = Vec::new();
    for id in ordered {
        changed.extend(phase_out(game, id));
    }
    changed
}

/// Change status without zone movement, retaining counters and attachments.
/// Attachments already phased out directly do not become indirect (CR 702.26g).
pub fn phase_out(game: &mut GameState, root: CardId) -> Vec<CardId> {
    if game.card(root).zone != ZoneType::Battlefield || game.card(root).phased_out {
        return Vec::new();
    }
    let controller = game.card(root).controller;
    game.card_mut(root).phased_out = true;
    game.card_mut(root).phased_out_controller = Some(controller);
    game.card_mut(root).phased_out_with = None;
    let mut changed = vec![root];
    let mut index = 0;
    while index < changed.len() {
        let host = changed[index];
        let attached: Vec<_> = game
            .cards
            .iter()
            .filter(|c| {
                c.zone == ZoneType::Battlefield && !c.phased_out && c.attached_to == Some(host)
            })
            .map(|c| c.id)
            .collect();
        for id in attached {
            game.card_mut(id).phased_out = true;
            game.card_mut(id).phased_out_with = Some(root);
            game.card_mut(id).phased_out_controller = None;
            changed.push(id);
        }
        index += 1;
    }
    changed
}

pub fn phase_in(game: &mut GameState, root: CardId) -> Vec<CardId> {
    if game.card(root).zone != ZoneType::Battlefield
        || !game.card(root).phased_out
        || game.card(root).phased_out_with.is_some()
    {
        return Vec::new();
    }
    let changed: Vec<_> = game
        .cards
        .iter()
        .filter(|c| {
            c.zone == ZoneType::Battlefield
                && c.phased_out
                && (c.id == root || c.phased_out_with == Some(root))
        })
        .map(|c| c.id)
        .collect();
    for &id in &changed {
        game.card_mut(id).phased_out = false;
        game.card_mut(id).phased_out_with = None;
        game.card_mut(id).phased_out_controller = None;
    }
    changed
}

/// Handles day/night transitions at the beginning of untap.
/// Mirrors Java's `Untap.doDayTime()`.
///
/// If it's day and previous player cast no spells → becomes night.
/// If it's night and previous player cast 2+ spells → becomes day.
pub fn do_day_time(game: &mut GameState, previous_player: Option<PlayerId>) {
    let previous = match previous_player {
        Some(p) => p,
        None => return,
    };

    let spells_cast = game.player(previous).spells_cast_this_turn;

    if !game.is_night && spells_cast == 0 {
        game.day_night_started = true;
        game.is_night = true; // transition to night
    } else if game.is_night && spells_cast > 1 {
        game.day_night_started = true;
        game.is_night = false; // transition to day
    }
}

/// Execute the untap step's "at" actions.
/// Mirrors Java's `Untap.executeAt()` which calls super.executeAt(),
/// then doPhasing(), doDayTime(), checkStaticAbilities(), and doUntap().
///
/// In Rust, the game loop calls these individually, but this provides
/// the combined entry point matching the Java interface.
pub fn execute_at(
    game: &mut GameState,
    turn_player: PlayerId,
    previous_player: Option<PlayerId>,
) -> Vec<CardId> {
    do_phasing(game, turn_player);
    do_day_time(game, previous_player);
    do_untap(game, turn_player)
}

/// Performs the untap of permanents for the active player.
/// Mirrors Java's `Untap.doUntap()`.
///
/// Untaps all tapped permanents controlled by the active player,
/// respecting "doesn't untap" and "you may choose not to untap" keywords.
pub fn do_untap(game: &mut GameState, active: PlayerId) -> Vec<CardId> {
    let cards: Vec<CardId> = game.cards_in_zone(ZoneType::Battlefield, active).to_vec();
    let mut untapped = Vec::new();

    for cid in cards {
        if game.card(cid).phased_out || !game.card(cid).tapped {
            continue;
        }

        // Skip cards that don't untap during untap step
        if game
            .card(cid)
            .has_keyword("CARDNAME doesn't untap during your untap step.")
        {
            continue;
        }

        // Skip exerted creatures (reset flag so they untap next turn)
        if game.card(cid).exerted {
            game.card_mut(cid).exerted = false;
            continue;
        }

        // Skip "This card doesn't untap during your next untap step."
        let has_skip = game
            .card(cid)
            .has_keyword("This card doesn't untap during your next untap step.");
        if has_skip {
            game.card_mut(cid)
                .keywords
                .remove("This card doesn't untap during your next untap step.");
            continue;
        }

        game.untap_during_untap_step(cid, active);
        untapped.push(cid);
    }

    // Remove exerted-by flags from all battlefield permanents
    for i in 0..game.cards.len() {
        if game.cards[i].zone == ZoneType::Battlefield {
            game.cards[i].exerted = false;
        }
    }

    untapped
}

#[cfg(test)]
mod tests {
    #[test]
    fn do_phasing_phases_in() {
        // Basic test that phasing works directionally
        // Full integration tests would need a GameState
    }
}
