//! Picking which lands pay a coloured cost.
//!
//! The engine's automatic payer taps sources one at a time and does not back up: a land that
//! makes any of three colours can be spent on the first pip it sees, leaving the last colour
//! unpayable although a different assignment pays the whole cost (DEV-086: Kaalia of the Vast,
//! `{1}{R}{W}{B}`). When a flexible source is among the offered ones, the bot searches for a
//! full assignment itself and taps the sources for the coloured pips one by one, leaving the
//! generic part to the automatic payer.
//!
//! The same assignment search (`assign`) answers "can these coloured pips be paid at all" for the
//! pre-cast affordability check (`coverable`), so a cast is not started that no tap order can pay
//! (DEV-091: a `{G}{W}{U}{B}` cost with one land supplying both green and blue).

use manabrew_protocol::game::ManaColor;
use manabrew_protocol::prompts::common::{PaymentAction, PaymentActionKind};

/// One way a source can be tapped: the action to send, and what it makes.
#[derive(Clone)]
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
    // A yield that depends on the game ("Amount$ Y": one per charge counter) is not the listed
    // amount, and may be none at all; such a source is left to the automatic payer.
    if let Some(amount) = info.description.split("Amount$").nth(1).map(|rest| rest.split('|').next().unwrap_or("").trim()) {
        if amount.parse::<i32>().is_err() {
            return None;
        }
    }
    let letter = action.id.split(':').nth(3).and_then(color_of);
    let made = info.produced_mana.as_ref()?.first()?;
    let color = letter.unwrap_or(made.color);
    Some((info.card_id.clone(), SourceTap { action_id: action.id.clone(), color, amount: made.amount.max(1) }))
}

/// A coloured pip: the colours that can pay it, one for `{R}`, two for the hybrid `{R/W}`.
type Pip = Vec<ManaColor>;

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
/// for a cost with anything else in it (X, Phyrexian, `{2/W}`, snow), which the automatic
/// payer keeps.
fn unpaid(cost: &str, floating: &[(ManaColor, i32)]) -> Option<(Vec<Pip>, i32)> {
    let mut pips: Vec<Pip> = Vec::new();
    let mut generic = 0;
    for symbol in cost.split('{').skip(1).map(|part| part.split('}').next().unwrap_or("")) {
        if let Ok(amount) = symbol.parse::<i32>() {
            generic += amount;
        } else {
            pips.push(symbol.split('/').map(color_of).collect::<Option<Pip>>()?);
        }
    }
    let mut spare = 0;
    for &(color, amount) in floating {
        let mut left = amount;
        while left > 0 {
            // The pip with the fewest ways to be paid takes the floating mana first.
            let at = pips.iter().enumerate().filter(|(_, pip)| pip.contains(&color)).min_by_key(|(_, pip)| pip.len()).map(|(at, _)| at);
            match at {
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

/// Whether `order` can be paid from distinct `sources`, with `generic` more mana left over from
/// the unused ones. Fills `chosen` with the action for each pip, in order.
fn assign<'a>(order: &[Pip], sources: &'a [(String, Vec<SourceTap>)], taken: &mut Vec<bool>, chosen: &mut Vec<&'a str>, generic: i32) -> bool {
    let Some((pip, rest)) = order.split_first() else {
        let spare: i32 = sources.iter().zip(taken.iter()).filter(|(_, used)| !**used)
            .map(|((_, options), _)| options.iter().map(|o| o.amount).max().unwrap_or(0)).sum();
        return spare >= generic;
    };
    for (index, (_, options)) in sources.iter().enumerate() {
        let Some(option) = options.iter().find(|o| pip.contains(&o.color)) else { continue };
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

/// The coloured pips of a cost, `None` when the cost has a symbol this does not read.
pub fn cost_pips(cost: &str) -> Option<Vec<Vec<ManaColor>>> {
    unpaid(cost, &[]).map(|(pips, _)| pips)
}

/// Whether every coloured pip can be paid from its own source, where each source is the colours
/// one tap can make. Optimistic about what a source makes, so a `false` is a certain no.
pub fn coverable(pips: &[Vec<ManaColor>], supplies: &[Vec<ManaColor>]) -> bool {
    let sources: Vec<(String, Vec<SourceTap>)> = supplies.iter().map(|colors| {
        (String::new(), colors.iter().map(|&color| SourceTap { action_id: String::new(), color, amount: 1 }).collect())
    }).collect();
    let mut order = pips.to_vec();
    order.sort_by_key(|pip| pip.len());
    assign(&order, &sources, &mut vec![false; sources.len()], &mut Vec::new(), 0)
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
    let capable = |pip: &Pip| sources.iter().filter(|(_, options)| options.iter().any(|o| pip.contains(&o.color))).count();
    let mut order = pips.clone();
    order.sort_by_key(|pip| capable(pip));
    sources.sort_by_key(|(_, options)| options.len());
    // Sources with one colour only cannot be wasted. When they alone pay every pip, the
    // automatic payer cannot strand a colour, so it keeps the payment.
    let fixed: Vec<_> = sources.iter().filter(|(_, options)| !flexible(options)).cloned().collect();
    if assign(&order, &fixed, &mut vec![false; fixed.len()], &mut Vec::new(), 0) {
        return None;
    }
    let mut taken = vec![false; sources.len()];
    let mut chosen: Vec<&str> = Vec::new();
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
        assert_eq!(planned_tap("{R/P}{B}", &[], &kaalia_sources(), &skip), None);
        assert_eq!(planned_tap("{2/W}{B}", &[], &kaalia_sources(), &skip), None);
        assert_eq!(planned_tap("{3}", &[], &kaalia_sources(), &skip), None, "generic cost: the automatic payer");
    }

    /// A flexible land is no reason to take over when single-colour lands pay every pip.
    #[test]
    fn the_automatic_payer_keeps_a_cost_the_fixed_lands_pay() {
        let skip = Default::default();
        let mut sources = vec![tap("tap:card-1:0", "card-1", 0, "R", 1), tap("tap:card-2:0", "card-2", 0, "W", 1)];
        sources.extend(kaalia_sources().into_iter().filter(|a| a.id.starts_with("tap:card-286")));
        assert_eq!(planned_tap("{R}{W}", &[], &sources, &skip), None);
        assert!(planned_tap("{R}{W}{B}", &[], &sources, &skip).is_some(), "black needs the flexible land");
    }

    #[test]
    fn an_unpayable_cost_is_left_to_the_automatic_payer() {
        let skip = Default::default();
        let sources = vec![tap("tap:card-1:0", "card-1", 0, "W", 1), tap("tap:card-1:1", "card-1", 1, "R", 1)];
        assert_eq!(planned_tap("{R}{W}", &[], &sources, &skip), None, "one land cannot pay two pips");
    }

    use ManaColor::{Black as B, Blue as U, Green as G, Red as R, White as W};

    /// The colours each source of a pod can make, one tap each.
    fn dual(a: ManaColor, b: ManaColor) -> Vec<ManaColor> { vec![a, b] }
    fn pips(cost: &str) -> Vec<Vec<ManaColor>> { cost_pips(cost).unwrap() }

    /// Follows the plan to the end, tapping what it names, and returns the colours paid.
    fn pay(cost: &str, mut actions: Vec<PaymentAction>) -> Option<Vec<ManaColor>> {
        let skip = Default::default();
        let mut floating: Vec<(ManaColor, i32)> = Vec::new();
        for _ in 0..8 {
            let Some(id) = planned_tap(cost, &floating, &actions, &skip) else { break };
            let action = actions.iter().find(|a| a.id == id)?;
            let PaymentActionKind::ActivateManaAbility(info) = &action.kind else { return None };
            let color = id.split(':').nth(3).and_then(color_of).unwrap_or(info.produced_mana.as_ref()?[0].color);
            floating.push((color, 1));
            let card = info.card_id.clone();
            actions.retain(|a| !matches!(&a.kind, PaymentActionKind::ActivateManaAbility(i) if i.card_id == card));
        }
        // When the plan stops, the automatic payer takes over; it must be able to finish with
        // single-colour sources alone, which it cannot strand.
        let (left, _) = unpaid(cost, &floating)?;
        let mut cards: Vec<(String, Vec<ManaColor>)> = Vec::new();
        for action in &actions {
            let (card, option) = option_of(action)?;
            match cards.iter_mut().find(|(id, _)| *id == card) {
                Some((_, colors)) => colors.push(option.color),
                None => cards.push((card, vec![option.color])),
            }
        }
        let fixed: Vec<_> = cards.into_iter().filter(|(_, colors)| colors.iter().all(|c| *c == colors[0])).map(|(_, colors)| colors).collect();
        let mut paid: Vec<ManaColor> = floating.into_iter().map(|(c, _)| c).collect();
        if !left.is_empty() {
            if !coverable(&left, &fixed) {
                return None;
            }
            paid.extend(left.iter().map(|pip| pip[0]));
        }
        Some(paid)
    }

    fn land(card: &str, colors: &[&str]) -> Vec<PaymentAction> {
        colors.iter().enumerate().map(|(i, c)| tap(&format!("tap:{card}:{i}"), card, i, c, 1)).collect()
    }

    /// An any-colour source lists one action per colour, the colour as the id's last part.
    fn any_colour(card: &str) -> Vec<PaymentAction> {
        ["W", "U", "B", "R", "G"].iter().map(|c| tap(&format!("tap:{card}:0:{c}"), card, 0, c, 1)).collect()
    }

    /// DEV-091: one three-colour land cannot pay both green and blue.
    #[test]
    fn four_colours_with_one_shared_land_is_not_coverable() {
        let spara = vec![W, U, G];
        let shrine = dual(W, B);
        let plains = vec![W];
        assert!(!coverable(&pips("{G}{W}{U}{B}"), &[spara.clone(), shrine.clone(), plains.clone()]));
        let island = vec![U];
        assert!(coverable(&pips("{G}{W}{U}{B}"), &[spara, shrine, plains, island]));
    }

    #[test]
    fn two_colours_need_two_sources() {
        assert!(coverable(&pips("{W}{U}"), &[dual(W, U), vec![W]]));
        assert!(!coverable(&pips("{W}{U}"), &[dual(W, U)]));
        assert!(!coverable(&pips("{W}{U}"), &[vec![W], vec![W]]));
    }

    #[test]
    fn three_colours_use_the_least_flexible_source_for_the_scarce_colour() {
        let supplies = [vec![W, U, B], vec![W], vec![U]];
        assert!(coverable(&pips("{W}{U}{B}"), &supplies));
        assert!(!coverable(&pips("{W}{U}{B}{B}"), &supplies));
    }

    #[test]
    fn five_colours_from_five_sources_each_making_two() {
        let ring = [dual(W, U), dual(U, B), dual(B, R), dual(R, G), dual(G, W)];
        assert!(coverable(&pips("{W}{U}{B}{R}{G}"), &ring));
        assert!(!coverable(&pips("{W}{U}{B}{R}{G}"), &ring[..4]));
        let wheel = [dual(W, U), dual(W, U), dual(B, R), dual(B, R), dual(R, G)];
        assert!(coverable(&pips("{W}{U}{B}{R}{G}"), &wheel));
    }

    #[test]
    fn hybrid_pips_take_either_colour() {
        assert!(coverable(&pips("{R/W}{R/W}"), &[vec![R], vec![W]]));
        assert!(!coverable(&pips("{R/W}{R/W}{B}"), &[vec![R], vec![W]]));
        assert!(coverable(&pips("{G/U}{B}"), &[vec![U], dual(B, G)]));
        assert!(!coverable(&pips("{G/U}{U/B}{B/G}"), &[dual(U, G), dual(U, G)]));
    }

    #[test]
    fn a_source_that_makes_any_colour_fills_the_missing_one() {
        let any = vec![W, U, B, R, G];
        assert!(coverable(&pips("{W}{U}{B}{R}{G}"), &[vec![W], vec![U], vec![B], vec![R], any.clone()]));
        assert!(!coverable(&pips("{W}{U}{B}{R}{G}"), &[vec![W], vec![U], vec![B], any.clone(), any.clone()][..4]));
        assert!(coverable(&pips("{W}{U}{B}{R}{G}"), &[any.clone(), any.clone(), any.clone(), any.clone(), any]));
    }

    #[test]
    fn colourless_and_floating_pips() {
        assert_eq!(cost_pips("{2}{G}{G}").unwrap().len(), 2);
        assert_eq!(cost_pips("{X}{G}"), None);
        assert_eq!(cost_pips("{G/P}"), None);
        let (left, generic) = unpaid("{1}{R/W}{B}", &[(W, 1), (G, 1)]).unwrap();
        assert_eq!((left, generic), (vec![vec![B]], 0));
    }

    /// The plan, followed tap by tap, pays a cost whose only solution gives the shared lands to
    /// different colours.
    #[test]
    fn the_plan_pays_four_colours_from_shared_lands() {
        let mut actions = land("card-1", &["W", "U", "G"]);
        actions.extend(land("card-2", &["W", "B"]));
        actions.extend(land("card-3", &["W"]));
        actions.extend(land("card-4", &["U"]));
        let paid = pay("{G}{W}{U}{B}", actions).expect("payable");
        for want in [G, W, U, B] {
            assert!(paid.contains(&want), "{paid:?}");
        }
    }

    #[test]
    fn the_plan_pays_five_colours_with_an_any_colour_source() {
        let mut actions = land("card-1", &["W", "U"]);
        actions.extend(land("card-2", &["B", "R"]));
        actions.extend(any_colour("card-3"));
        actions.extend(land("card-4", &["W"]));
        actions.extend(land("card-5", &["R"]));
        let paid = pay("{W}{U}{B}{R}{G}", actions).expect("payable");
        assert_eq!(paid.len(), 5);
        for want in [W, U, B, R, G] {
            assert!(paid.contains(&want), "{paid:?}");
        }
    }

    #[test]
    fn the_plan_pays_a_hybrid_cost() {
        let mut actions = land("card-1", &["R", "G"]);
        actions.extend(land("card-2", &["W", "G"]));
        actions.extend(land("card-3", &["B"]));
        let paid = pay("{R/W}{G/W}{B}", actions).expect("payable");
        assert!(paid.contains(&B));
        assert_eq!(paid.len(), 3);
    }

    /// Astral Cornucopia: "Amount$ Y", one mana per charge counter, none without counters. The
    /// listed amount is not trusted, so the source is not planned with.
    #[test]
    fn a_source_with_a_variable_yield_is_not_planned_with() {
        let variable: PaymentAction = serde_json::from_value(json!({"id":"tap:card-2:0:U","type":"activateManaAbility","cardId":"card-2","abilityIndex":0,
            "description":"AB$ Mana | Cost$ T | Produced$ Any | Amount$ Y | SpellDescription$ Choose a color.","isManaAbility":true,"cost":"{T}",
            "producedMana":[{"amount":1,"color":"U"}]})).unwrap();
        assert!(option_of(&variable).is_none());
        let counted: PaymentAction = serde_json::from_value(json!({"id":"tap:card-9:0","type":"activateManaAbility","cardId":"card-9","abilityIndex":0,
            "description":"AB$ Mana | Cost$ T | Produced$ C | Amount$ 2 | SpellDescription$ Add {C}{C}.","isManaAbility":true,"cost":"{T}",
            "producedMana":[{"amount":2,"color":"C"}]})).unwrap();
        assert!(option_of(&counted).is_some());
    }
}
