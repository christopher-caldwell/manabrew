use forge_foundation::{CardTypeLine, ColorSet, ManaCost, ZoneType};
use manabrew_engine::{agent::{PlayerAgent, TargetChoice}, card::Card, combat::DefenderId, game::GameState, game_loop::GameLoop, ids::{CardId, PlayerId}, mana::ManaPool, player::actions::PlayerAction, spellability::{build_spell_ability, SpellAbility, StackEntry}};

struct Agent { copy: bool, retarget: bool, target: CardId }
impl PlayerAgent for Agent {
    fn mulligan_decision(&mut self, _: PlayerId, _: &[CardId], _: u32) -> bool { true }
    fn choose_action(&mut self, _: PlayerId, _: Option<&manabrew_engine::agent::PriorityActionSpace>, _: &mut dyn FnMut() -> manabrew_engine::agent::PriorityActionSpace) -> PlayerAction { PlayerAction::PassPriority }
    fn choose_attackers(&mut self, _: PlayerId, _: &[CardId], _: &[DefenderId]) -> Vec<(CardId, DefenderId)> { vec![] }
    fn choose_blockers(&mut self, _: PlayerId, _: &[CardId], _: &[CardId], _: Option<usize>) -> Vec<(CardId, CardId)> { vec![] }
    fn choose_targets_for(&mut self, sa: &mut SpellAbility, game: &GameState, pools: &[ManaPool]) -> bool { manabrew_engine::spellability::choose_targets_by_kind(self, sa, game, pools) }
    fn choose_target_player(&mut self, _: PlayerId, valid: &[PlayerId], _: Option<&SpellAbility>) -> Option<PlayerId> { valid.first().copied() }
    fn choose_target_card(&mut self, _: PlayerId, valid: &[CardId], _: Option<&SpellAbility>) -> Option<CardId> { valid.iter().copied().find(|id| *id == self.target) }
    fn choose_target_any(&mut self, _: PlayerId, _: &[PlayerId], _: &[CardId], _: Option<&SpellAbility>) -> TargetChoice { TargetChoice::None }
    fn choose_land_or_spell(&mut self, _: PlayerId) -> Option<bool> { None }
    fn confirm_action(&mut self, _: PlayerId, mode: Option<&str>, _: &str, _: &[String], _: Option<CardId>, _: Option<manabrew_engine::ability::api_type::ApiType>) -> bool {
        match mode { Some("OptionalCopy") => self.copy, Some("CopyRetarget") => self.retarget, _ => false }
    }
}

fn creature(game: &mut GameState, player: PlayerId, name: &str) -> CardId {
    let id = game.create_card(Card::new(CardId(0), name.into(), player, CardTypeLine::parse("Creature"), ManaCost::parse("1 W"), ColorSet::WHITE, Some(2), Some(2), vec![], vec![]));
    game.move_card(id, ZoneType::Graveyard, player); id
}

#[test]
fn reclamation_parent_copy_obeys_origin_optional_choice_and_new_target() {
    const SCRIPT: &str = "Name:Sevinne's Reclamation\nManaCost:2 W\nTypes:Sorcery\nA:SP$ ChangeZone | Origin$ Graveyard | Destination$ Battlefield | ValidTgts$ Permanent.cmcLE3+YouCtrl | SubAbility$ DBCopy\nSVar:DBCopy:DB$ CopySpellAbility | Defined$ Parent | Optional$ True | MayChooseTarget$ True | ConditionDefined$ Self | ConditionPresent$ Card.wasCastFromGraveyard\nK:Flashback:4 W\n";
    for (from, accept, retarget, expect_copy) in [(ZoneType::Hand,true,true,false),(ZoneType::Graveyard,false,true,false),(ZoneType::Graveyard,true,false,true),(ZoneType::Graveyard,true,true,true)] {
        let mut game = GameState::new(&["A","B"], 40); let p = PlayerId(0);
        let first = creature(&mut game,p,"First"); let second = creature(&mut game,p,"Second");
        let rules = forge_carddb::parse_card_script(SCRIPT).unwrap(); let id = game.create_card(Card::from_rules(&rules,p));
        game.move_card(id,ZoneType::Stack,p);
        let mut sa=build_spell_ability(&game,id,&game.card(id).abilities[0],p);
        sa.target_chosen.target_card=Some(first); sa.target_chosen.target_card_zone_timestamp=Some(game.card(first).zone_timestamp);
        if from==ZoneType::Graveyard { sa.alt_cost=Some(manabrew_engine::spellability::AlternativeCost::Flashback); }
        game.stack.push(StackEntry{id:0,spell_ability:sa,is_pending_cast:false,is_creature_spell:false,is_permanent_spell:false,cast_from_zone:Some(from),optional_trigger_decider:None,optional_trigger_description:None,optional_trigger_source_name:None});
        let mut agents:Vec<Box<dyn PlayerAgent>>=vec![Box::new(Agent{copy:accept,retarget,target:second}),Box::new(manabrew_engine::agent::PassAgent)];
        let mut gl=GameLoop::new(2); gl.resolve_stack(&mut game,&mut agents);
        assert_eq!(game.card(first).zone,ZoneType::Battlefield,"{from:?} {accept} {retarget}");
        assert_eq!(game.card(id).zone,if from==ZoneType::Graveyard { ZoneType::Exile } else { ZoneType::Graveyard });
        assert_eq!(!game.stack.is_empty(),expect_copy,"{from:?} {accept} {retarget}");
        if expect_copy {
            let copy=&game.stack.peek().unwrap().spell_ability;
            assert!(copy.is_copy); assert_eq!(copy.target_chosen.target_card,Some(if retarget { second } else { first }));
            gl.resolve_stack(&mut game,&mut agents);
            assert_eq!(game.card(second).zone,if retarget {ZoneType::Battlefield} else {ZoneType::Graveyard});
            assert!(game.stack.is_empty());
        }
    }
}
