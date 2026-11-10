use forge_foundation::ZoneType;

use super::EffectContext;
use crate::ability::ability_ir::DefinedRef;
use crate::card::card_util;
use crate::spellability::SpellAbility;

/// Return the list of qualities a `SP$ Protection | Gains$ Choice` ability can
/// choose from. Mirrors Java `ProtectEffect.getProtectionList(SpellAbility)`.
///
/// `AnyColor` expands to the five MTG colors and `CardType` to core card types.
/// Everything else is a literal
/// quality; never expose the script's category placeholder as a choice.
pub fn get_protection_list(sa: &SpellAbility) -> Vec<String> {
    let gains = sa.ir.gains.as_deref().unwrap_or("");
    if gains != "Choice" && !gains.contains("chosen color") {
        return gains
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    let mut out = Vec::new();
    let choices = sa.ir.choices.as_deref().unwrap_or("");
    for choice in choices.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match choice {
            "AnyColor" => out.extend(["White", "Blue", "Black", "Red", "Green"].map(String::from)),
            "CardType" => out.extend(
                forge_foundation::CoreType::ALL
                    .iter()
                    .map(|t| t.name().to_string()),
            ),
            _ => out.push(choice.to_string()),
        }
    }
    if out.is_empty() {
        out = ["White", "Blue", "Black", "Red", "Green"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    out
}

/// Use the type-choice interface for type qualities, retaining the existing
/// color interface for color choices. Both targeted and mass grants share it.
pub(super) fn choose_protection_keyword(
    ctx: &mut EffectContext,
    sa: &SpellAbility,
) -> Option<String> {
    let gains = sa.ir.gains.as_deref().unwrap_or_default();
    if !gains.contains("Choice") && !gains.contains("chosen color") {
        return Some(gains.to_string());
    }
    let choices = get_protection_list(sa);
    let agent = &mut ctx.agents[sa.activating_player.index()];
    let chosen = if sa
        .ir
        .choices
        .as_deref()
        .is_some_and(|s| s.split(',').any(|c| c.trim() == "CardType"))
    {
        agent.choose_type_for_source(sa.activating_player, "Card", &choices, sa.source)
    } else {
        agent.choose_color(sa.activating_player, &choices)
    };
    // Protection is a required choice. Guard malformed/cancelled agent replies
    // with a legal choice, rather than granting an unresolved keyword.
    let chosen = chosen
        .filter(|c| choices.contains(c))
        .or_else(|| choices.first().cloned())?;
    Some(format!("Protection from {}", chosen.to_lowercase()))
}

/// End-of-turn revert for Protection. Mirrors the `GameCommand.run()` in Java
/// `ProtectEffect` that removes the granted protection keyword when the
/// effect duration expires.
pub fn run(game: &mut crate::game::GameState, card_id: crate::ids::CardId, keyword: &str) {
    if game.card(card_id).zone == ZoneType::Battlefield {
        game.card_mut(card_id).pump_keywords.remove(keyword);
    }
}

/// `SP$ Protection` — grant protection from a quality to a permanent.
///
/// Mirrors Java's `ProtectEffect.java`.
/// - `Gains$` — the protection keyword to grant (e.g. "Protection from chosen color").
/// - `Choices$` — if present, player chooses what to protect from.
///
/// # Card script examples
/// ```text
/// A:SP$ Protection | Gains$ Protection from chosen color | Choices$ White,Blue,Black,Red,Green
/// A:SP$ Protection | Gains$ Protection from red
/// ```
/// Struct form of this effect so it can participate in the
/// `SpellAbilityEffect` trait hierarchy — mirrors Java's
/// `ProtectEffect` class extending `SpellAbilityEffect`.
#[manabrew_engine_macros::spell_effect(ProtectEffect)]
fn resolve(ctx: &mut EffectContext, sa: &crate::spellability::SpellAbility) {
    // Determine target
    let target = sa
        .target_chosen
        .target_card
        .or_else(|| match sa.defined_ref() {
            Some(DefinedRef::SelfCard) => sa.source,
            Some(DefinedRef::ParentTarget) => ctx.parent_target_card,
            _ => sa.source,
        });

    let mut targets = target.into_iter().collect::<Vec<_>>();
    targets.extend(card_util::get_radiance(ctx.game, sa).iter().copied());
    targets.retain(|&id| ctx.game.card(id).zone == ZoneType::Battlefield);
    targets.sort_unstable_by_key(|cid| cid.0);
    targets.dedup();
    if targets.is_empty() {
        return;
    }

    if let Some(keyword) = choose_protection_keyword(ctx, sa) {
        for card_id in targets {
            ctx.game.card_mut(card_id).add_pump_keyword(&keyword);
        }
    }
}
