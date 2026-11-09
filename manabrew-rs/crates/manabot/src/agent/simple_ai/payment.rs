//! Picking which lands pay a coloured cost.
//!
//! The engine's automatic payer taps sources one at a time and does not back up: a land that
//! makes any of three colours can be spent on the first pip it sees, leaving the last colour
//! unpayable although a different assignment pays the whole cost (DEV-086: Kaalia of the Vast,
//! `{1}{R}{W}{B}`). When a flexible source is among the offered ones, the bot searches for a
//! full assignment itself and taps the sources for the coloured pips one by one, leaving the
//! generic part to the automatic payer.

use manabrew_protocol::game::ManaColor;
use manabrew_protocol::prompts::common::{PaymentAction, PaymentActionKind};

/// One way a source can be tapped: the action to send, and what it makes.
struct SourceTap {
    action_id: String,
    color: ManaColor,
    amount: i32,
}

/// The mana colour a payment action makes. A source with a choice of colour has one action per
/// colour, whose id ends in the colour letter (`tap:card-280:1:R`); its `produced_mana` is not
/// reliable there, so the id wins.
fn option_of(action: &PaymentAction) -> Option<(String, SourceTap)> {
    let PaymentActionKind::ActivateManaAbility(info) = &action.kind else { return None };
    // Only a plain `{T}` source: costed ones (filter lands) have their own handling.
    if info.cost.as_deref().is_some_and(|cost| cost != "{T}") {
        return None;
    }
    let letter = action.id.split(':').nth(3).and_then(color_of);
    let made = info.produced_mana.as_ref()?.first()?;
    let color = letter.unwrap_or(made.color);
    Some((info.card_id.clone(), SourceTap { action_id: action.id.clone(), color, amount: made.amount.max(1) }))
}

fn color_of(symbol: &str) -> Option<ManaColor> {
    Some(match symbol {
        "W" => ManaColor::White,
        "U" => ManaColor::Blue,
        "B" => ManaColor::Black,
        "R" => ManaColor::Red,
        "G" => ManaColor::Green,
        "C" => ManaColor::Colorless,
        _ => return None,
    })
}

/// The coloured pips and generic amount of `cost` still unpaid after the floating mana. `None`
/// for a cost with anything else in it (X, hybrid, Phyrexian, snow), which the automatic
/// payer keeps.
fn unpaid(cost: &str, floating: &[(ManaColor, i32)]) -> Option<(Vec<ManaColor>, i32)> {
    let mut pips = Vec::new();
    let mut generic = 0;
    for symbol in cost.split('{').skip(1).map(|part| part.split('}').next().unwrap_or("")) {
        if let Ok(amount) = symbol.parse::<i32>() {
            generic += amount;
        } else {
            pips.push(color_of(symbol)?);
        }
    }
    let mut spare = 0;
    for &(color, amount) in floating {
        let mut left = amount;
        while left > 0 {
            match pips.iter().position(|pip| *pip == color) {
                Some(at) => {
                    pips.remove(at);
                    left -= 1;
                }
                None => break,
            }
        }
        spare += left;
    }
    Some((pips, (generic - spare).max(0)))
}

/// The next source to tap so that the cost stays payable, or `None` when the automatic payer
/// should go on: no coloured pip left, no source with a choice of colour, a cost this does not
/// read, or no assignment at all.
pub fn planned_tap(cost: &str, floating: &[(ManaColor, i32)], actions: &[PaymentAction], skip: &std::collections::HashSet<String>) -> Option<String> {
    let (pips, generic) = unpaid(cost, floating)?;
    if pips.is_empty() {
        return None;
    }
    let mut sources: Vec<(String, Vec<SourceTap>)> = Vec::new();
    for action in actions.iter().filter(|action| !skip.contains(&action.id)) {
        if let Some((card, option)) = option_of(action) {
            match sources.iter_mut().find(|(id, _)| *id == card) {
                Some((_, options)) => options.push(option),
                None => sources.push((card, vec![option])),
            }
        }
    }
    let flexible = |options: &Vec<SourceTap>| options.iter().any(|o| o.color != options[0].color);
    if !sources.iter().any(|(_, options)| flexible(options)) {
        return None;
    }
    // The pips fewest sources can make go first; within a pip, the least flexible source.
    let capable = |pip: &ManaColor| sources.iter().filter(|(_, options)| options.iter().any(|o| o.color == *pip)).count();
    let mut order = pips.clone();
    order.sort_by_key(|pip| capable(pip));
    sources.sort_by_key(|(_, options)| options.len());
    let mut taken = vec![false; sources.len()];
    let mut chosen: Vec<&str> = Vec::new();
    fn assign<'a>(order: &[ManaColor], sources: &'a [(String, Vec<SourceTap>)], taken: &mut Vec<bool>, chosen: &mut Vec<&'a str>, generic: i32) -> bool {
        let Some((pip, rest)) = order.split_first() else {
            let spare: i32 = sources.iter().zip(taken.iter()).filter(|(_, used)| !**used)
                .map(|((_, options), _)| options.iter().map(|o| o.amount).max().unwrap_or(0)).sum();
            return spare >= generic;
        };
        for (index, (_, options)) in sources.iter().enumerate() {
            let Some(option) = options.iter().find(|o| o.color == *pip) else { continue };
            if taken[index] {
                continue;
            }
            taken[index] = true;
            chosen.push(&option.action_id);
            // A source that makes more than the pip needs (a `{C}{C}` land) also pays generic.
            if assign(rest, sources, taken, chosen, (generic - (option.amount - 1)).max(0)) {
                return true;
            }
            chosen.pop();
            taken[index] = false;
        }
        false
    }
    if assign(&order, &sources, &mut taken, &mut chosen, generic) {
        chosen.first().map(|id| id.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tap(id: &str, card: &str, index: usize, color: &str, amount: i32) -> PaymentAction {
        serde_json::from_value(json!({"id":id,"type":"activateManaAbility","cardId":card,"abilityIndex":index,
            "description":"","isManaAbility":true,"cost":"{T}","producedMana":[{"amount":amount,"color":color}]})).unwrap()
    }

    /// Kaalia's payment prompt from the DEV-086 trace: a three-colour land, a white land, a
    /// `{C}{C}` land and a pain land that makes `{B}` or `{R}`.
    fn kaalia_sources() -> Vec<PaymentAction> {
        vec![
            tap("tap:card-286:0", "card-286", 0, "W", 1),
            tap("tap:card-286:1", "card-286", 1, "B", 1),
            tap("tap:card-286:2", "card-286", 2, "R", 1),
            tap("tap:card-296:0", "card-296", 0, "W", 1),
            tap("tap:card-227:0", "card-227", 0, "C", 2),
            tap("tap:card-280:0", "card-280", 0, "C", 1),
            tap("tap:card-280:1:B", "card-280", 1, "B", 1),
            // The engine lists black for this one too; the id says red.
            tap("tap:card-280:1:R", "card-280", 1, "B", 1),
        ]
    }

    /// DEV-086: the automatic payer spent the three-colour land on white and could not make red.
    /// Following the plan prompt by prompt pays R, W and B from three different lands.
    #[test]
    fn following_the_plan_pays_every_coloured_pip() {
        let skip = Default::default();
        let mut actions = kaalia_sources();
        let mut floating: Vec<(ManaColor, i32)> = Vec::new();
        let mut tapped: Vec<String> = Vec::new();
        while let Some(id) = planned_tap("{1}{R}{W}{B}", &floating, &actions, &skip) {
            let action = actions.iter().find(|a| a.id == id).unwrap();
            let PaymentActionKind::ActivateManaAbility(info) = &action.kind else { unreachable!() };
            let made = info.produced_mana.as_ref().unwrap()[0];
            let color = id.split(':').nth(3).and_then(color_of).unwrap_or(made.color);
            floating.push((color, 1));
            let card = info.card_id.clone();
            tapped.push(card.clone());
            actions.retain(|a| !matches!(&a.kind, PaymentActionKind::ActivateManaAbility(i) if i.card_id == card));
            assert!(tapped.len() <= 3, "{tapped:?}");
        }
        let colors: Vec<_> = floating.iter().map(|(c, _)| *c).collect();
        for want in [ManaColor::Red, ManaColor::White, ManaColor::Black] {
            assert!(colors.contains(&want), "{colors:?}");
        }
        tapped.sort();
        tapped.dedup();
        assert_eq!(tapped.len(), 3, "three different lands");
        assert!(actions.iter().any(|a| a.id == "tap:card-227:0"), "the {{C}}{{C}} land is left for the generic part");
    }

    #[test]
    fn nothing_is_planned_without_a_flexible_source_or_with_an_unreadable_cost() {
        let skip = Default::default();
        let plain = vec![tap("tap:card-1:0", "card-1", 0, "W", 1), tap("tap:card-2:0", "card-2", 0, "R", 1)];
        assert_eq!(planned_tap("{R}{W}", &[], &plain, &skip), None);
        assert_eq!(planned_tap("{X}{R}", &[], &kaalia_sources(), &skip), None);
        assert_eq!(planned_tap("{R/W}{B}", &[], &kaalia_sources(), &skip), None);
        assert_eq!(planned_tap("{3}", &[], &kaalia_sources(), &skip), None, "generic cost: the automatic payer");
    }

    #[test]
    fn an_unpayable_cost_is_left_to_the_automatic_payer() {
        let skip = Default::default();
        let sources = vec![tap("tap:card-1:0", "card-1", 0, "W", 1), tap("tap:card-1:1", "card-1", 1, "R", 1)];
        assert_eq!(planned_tap("{R}{W}", &[], &sources, &skip), None, "one land cannot pay two pips");
    }
}
