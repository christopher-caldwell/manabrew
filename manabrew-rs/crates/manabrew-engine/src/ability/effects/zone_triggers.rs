//! Zone trigger emission helpers.
//!
//! Functions for firing ChangesZone triggers when cards move between zones.
//! Used by multiple zone-change effects (ChangeZone, ChangeZoneAll, Sacrifice, etc.).

use forge_foundation::ZoneType;

use crate::event::RunParams;
use crate::ids::CardId;
use crate::game::GameState;
use crate::trigger::handler::TriggerHandler;
use crate::trigger::TriggerType;

/// Emit a ChangesZone trigger event. Used by multiple zone-moving effects.
pub fn emit_zone_trigger(
    trigger_handler: &mut TriggerHandler,
    game: &GameState,
    card_id: CardId,
    origin: ZoneType,
    destination: ZoneType,
) {
    trigger_handler.run_trigger(
        TriggerType::ChangesZone,
        RunParams {
            card: Some(card_id),
            card_lki: Some(card_id),
            lki_attachment_captured: origin == ZoneType::Battlefield,
            lki_attached_to: if origin == ZoneType::Battlefield {
                let card = game.card(card_id);
                if card.zone == ZoneType::Battlefield {
                    card.attached_to.map(|id| (id, game.card(id).zone_timestamp))
                } else {
                    card.lki_attached_to
                }
            } else { None },
            origin: Some(origin),
            destination: Some(destination),
            ..Default::default()
        },
        false,
    );
}

/// Like `emit_zone_trigger` but carries LKI +1/+1 counter count.
/// Used when a creature with Modular dies so the death trigger can move
/// the correct number of counters (not just the static Modular:N value).
pub fn emit_zone_trigger_with_lki_counters(
    trigger_handler: &mut TriggerHandler,
    game: &GameState,
    card_id: CardId,
    origin: ZoneType,
    destination: ZoneType,
    lki_p1p1_counters: i32,
    lki_power: i32,
    lki_toughness: i32,
) {
    trigger_handler.run_trigger(
        TriggerType::ChangesZone,
        RunParams {
            card: Some(card_id),
            card_lki: Some(card_id),
            lki_attachment_captured: origin == ZoneType::Battlefield,
            lki_attached_to: if origin == ZoneType::Battlefield {
                let card = game.card(card_id);
                if card.zone == ZoneType::Battlefield {
                    card.attached_to.map(|id| (id, game.card(id).zone_timestamp))
                } else {
                    card.lki_attached_to
                }
            } else { None },
            origin: Some(origin),
            destination: Some(destination),
            lki_p1p1_counters: Some(lki_p1p1_counters),
            lki_power: Some(lki_power),
            lki_toughness: Some(lki_toughness),
            ..Default::default()
        },
        false,
    );
}
