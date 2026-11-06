use forge_foundation::{ CardTypeLine, ColorSet, ManaCost, ZoneType};
use manabrew_engine::{
    agent::{ManaAbilityOption, ManaCostAction, PassAgent, PlayOption, PlayerAgent, PriorityActionSpace, TargetChoice},
    card::Card, combat::DefenderId, game::GameState, game_loop::GameLoop,
    ids::{CardId, PlayerId}, mana::ManaPool, player::actions::{PlayerAction, AbilityRef},
    spellability::SpellAbility,
};
use std::{cell::Cell, rc::Rc};

struct CastingAgent { spell: CardId, shepherd: CardId, creature: CardId, second: CardId, step: usize, payment_step: usize, entered: Rc<Cell<bool>> }
impl PlayerAgent for CastingAgent {
    fn choose_number(&mut self, _: PlayerId, _: Option<CardId>, _: &str, _: Option<&str>, _: i32, max: i32) -> Option<i32> { Some(max) }
    fn choose_land_or_spell(&mut self, _: PlayerId) -> Option<bool> { None }
    fn mulligan_decision(&mut self, _: PlayerId, _: &[CardId], _: u32) -> bool { true }
    fn choose_action(&mut self, _: PlayerId, _: Option<&PriorityActionSpace>, _: &mut dyn FnMut() -> PriorityActionSpace) -> PlayerAction {
        match self.step { 0 => { self.step += 1; PlayerAction::ActivateAbility(AbilityRef { card_id: self.shepherd, ability_index: 0 }) }, 1 => { self.step += 1; PlayerAction::CastSpell(PlayOption::normal(self.spell)) }, _ => PlayerAction::PassPriority }
    }
    fn choose_attackers(&mut self, _: PlayerId, _: &[CardId], _: &[DefenderId]) -> Vec<(CardId, DefenderId)> { vec![] }
    fn choose_blockers(&mut self, _: PlayerId, _: &[CardId], _: &[CardId], _: Option<usize>) -> Vec<(CardId, CardId)> { vec![] }
    fn choose_targets_for(&mut self, sa: &mut SpellAbility, game: &GameState, pools: &[ManaPool]) -> bool { manabrew_engine::spellability::choose_targets_by_kind(self, sa, game, pools) }
    fn choose_target_player(&mut self, _: PlayerId, valid: &[PlayerId], _: Option<&SpellAbility>) -> Option<PlayerId> { valid.first().copied() }
    fn choose_target_card(&mut self, _: PlayerId, valid: &[CardId], _: Option<&SpellAbility>) -> Option<CardId> { valid.first().copied() }
    fn choose_target_any(&mut self, _: PlayerId, _: &[PlayerId], _: &[CardId], _: Option<&SpellAbility>) -> TargetChoice { TargetChoice::None }
    fn pay_mana_cost(&mut self, _: PlayerId, _: CardId, _: &str, _: &str, _: &str, _: &str, _: bool, _: bool, _: &[CardId], _: &[ManaAbilityOption], _: &[CardId], _: &[CardId], _: &ManaPool) -> ManaCostAction {
        self.entered.set(true);
        if self.payment_step == 0 { self.payment_step += 1; return ManaCostAction::Pay { auto: true }; }
        let action = match self.payment_step {
            1 => ManaCostAction::TapForMana { card_id: self.creature, mana_ability_index: Some(usize::MAX), express_choice: None },
            2 => ManaCostAction::TapForMana { card_id: self.second, mana_ability_index: Some(usize::MAX), express_choice: None },
            _ => ManaCostAction::Pay { auto: true },
        };
        self.payment_step += 1;
        action
    }
}

#[test]
fn failed_payment_retracts_convoke_after_mana_effect() {
    std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
        let p = PlayerId(0);
        let mut game = GameState::new(&["A", "B"], 40);
        let mut add = |card: Card, zone| {
            let id = game.create_card(card);
            game.move_card(id, zone, p);
            id
        };
        let make = |name: &str, types: &str, cost: &str, color, keywords: Vec<String>, abilities: Vec<String>| {
            Card::new(CardId(0), name.into(), p, CardTypeLine::parse(types), ManaCost::parse(cost), color, Some(2), Some(2), keywords, abilities)
        };
        let spell = add(make("Fixture", "Instant", "X G G G", ColorSet::GREEN, vec!["Convoke".into()], vec!["SP$ GainLife | Defined$ You | LifeAmount$ 1".into()]), ZoneType::Hand);
        let mut archdruid = make("Elvish Archdruid", "Creature - Elf Druid", "1 G G", ColorSet::GREEN, vec![], vec!["AB$ Mana | Cost$ T | Produced$ G | Amount$ X".into()]);
        archdruid.svars.insert("X".into(), "Count$Valid Elf.YouCtrl".into());
        let creature = add(archdruid, ZoneType::Battlefield);
        let second = add(make("Helper Two", "Creature - Elf", "G", ColorSet::GREEN, vec![], vec!["AB$ Mana | Cost$ T | Produced$ G".into()]), ZoneType::Battlefield);
        for _ in 0..12 { add(make("Elf", "Creature - Elf", "G", ColorSet::GREEN, vec![], vec![]), ZoneType::Battlefield); }
        let shepherd = add(make("Allosaurus Shepherd", "Creature - Elf Shaman", "G", ColorSet::GREEN, vec![], vec!["AB$ PumpAll | Cost$ 4 G G | ValidCards$ Creature.Elf+YouCtrl | NumAtt$ 0 | NumDef$ 0".into()]), ZoneType::Battlefield);
        for _ in 0..6 { add(make("Forest", "Land - Forest", "0", ColorSet::COLORLESS, vec![], vec!["AB$ Mana | Cost$ T | Produced$ G".into()]), ZoneType::Battlefield); }
        let mut land_card = make("Draw Land", "Land", "0", ColorSet::COLORLESS, vec![], vec!["AB$ Mana | Cost$ T | Produced$ G | SubAbility$ DBDraw".into()]);
        land_card.svars.insert("DBDraw".into(), "DB$ Draw | Defined$ You | NumCards$ 1".into());
        let land = add(land_card, ZoneType::Battlefield);
        let guidance = add(Card::from_rules(&forge_carddb::parse_card_script("Name:Elvish Guidance\nManaCost:2 G\nTypes:Enchantment Aura\nK:Enchant:Land\nT:Mode$ TapsForMana | ValidCard$ Card.EnchantedBy | Execute$ TrigMana\nSVar:TrigMana:DB$ Mana | Produced$ G | Amount$ X\nSVar:X:Count$Valid Creature.Elf").unwrap(), p), ZoneType::Battlefield);
        let drawn = add(make("Drawn", "Creature - Human", "0", ColorSet::COLORLESS, vec![], vec![]), ZoneType::Library);
        game.card_mut(guidance).attached_to = Some(land);
        game.card_mut(shepherd).add_replacement_effect(manabrew_engine::replacement::replacement_effect::ReplacementEffect::new(manabrew_engine::replacement::replacement_effect::ReplacementType::Counter, manabrew_engine::replacement::replacement_effect::ReplacementLayer::CantHappen, manabrew_engine::parsing::Params::from_raw("ValidCard$ Card.Self"), vec![ZoneType::Battlefield]));
        game.card_mut(creature).set_summoning_sick(false);
        game.card_mut(second).set_summoning_sick(false);
        let entered = Rc::new(Cell::new(false));
        let mut agents: Vec<Box<dyn PlayerAgent>> = vec![Box::new(CastingAgent { spell, shepherd, creature, second, step: 0, payment_step: 0, entered: entered.clone() }), Box::new(PassAgent)];
        let mut driver = GameLoop::new(2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| driver.priority_round(&mut game, &mut agents, true)));
        assert!(result.is_ok());
        assert!(entered.get(), "cast must reach mana payment");
        assert_eq!(game.card(spell).zone, ZoneType::Hand);
        assert!(!game.card(creature).tapped);
        assert!(game.card(land).tapped);
        assert_eq!(game.card(drawn).zone, ZoneType::Hand);
        assert_eq!(driver.pool(p).total_mana(), 2);
        assert!(!game.card(second).tapped);
        assert!(game.stack.iter().all(|entry| entry.spell_ability.source != Some(spell)));
    }).unwrap().join().unwrap();
}
