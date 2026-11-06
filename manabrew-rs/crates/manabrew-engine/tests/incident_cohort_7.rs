use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
use manabrew_engine::{card::Card, game::GameState, ids::{CardId, PlayerId}, svar::resolve_count_svar};

#[test]
fn count_dual_devotion_and_attackers() {
    let p = PlayerId(0);
    let q = PlayerId(1);
    let mut game = GameState::new(&["A", "B", "C"], 40);
    let mut add = |owner, types: &str, cost: &str| {
        let card = Card::new(CardId(0), "Fixture".into(), owner, CardTypeLine::parse(types), ManaCost::parse(cost), ColorSet::COLORLESS, Some(1), Some(1), vec![], vec![]);
        let id = game.create_card(card);
        game.move_card(id, ZoneType::Battlefield, owner);
        id
    };
    let source = add(p, "Enchantment", "R W R/W");
    let phased = add(p, "Creature", "R R");
    let _opposing = add(q, "Creature", "W W");
    let attacker = add(q, "Creature", "0");
    let other = add(q, "Creature", "0");
    game.card_mut(phased).phased_out = true;
    game.card_mut(attacker).attacking_player = Some(p);
    game.card_mut(other).attacking_player = Some(PlayerId(2));
    let devotion = resolve_count_svar("Count$DevotionDual.Red.White", &game, source, p);
    let attacks = resolve_count_svar("Count$ValidAll Creature.attackingYouOrYourPWLKI", &game, source, p);
    assert_eq!((devotion, attacks), (3, 1));
    game.card_mut(attacker).attacking_player = None;
    assert_eq!(resolve_count_svar("Count$ValidAll Creature.attackingYouOrYourPWLKI", &game, source, p), 0);
    assert_eq!(resolve_count_svar("Count$DevotionDual.Red.White", &game, source, q), 2);
}
