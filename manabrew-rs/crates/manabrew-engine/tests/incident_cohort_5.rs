use forge_foundation::ZoneType;
use manabrew_engine::{agent::{PlayerAgent, PriorityActionSpace, ManaCostAction, ManaAbilityOption, TargetChoice}, card::Card, combat::DefenderId, game::GameState, game_loop::GameLoop, ids::{CardId, PlayerId}, mana::ManaPool, player::actions::PlayerAction, spellability::SpellAbility};

struct Agent { spell: CardId, helpers: [CardId; 2], action: bool, payment: usize }
impl PlayerAgent for Agent {
 fn mulligan_decision(&mut self, _:PlayerId, _: &[CardId], _:u32)->bool { true }
 fn choose_action(&mut self, _:PlayerId, space:Option<&PriorityActionSpace>, request:&mut dyn FnMut()->PriorityActionSpace)->PlayerAction {
  if self.action { return PlayerAction::PassPriority; } self.action=true;
  let owned; let space=match space {Some(s)=>s,None=>{owned=request(); &owned}};
  PlayerAction::CastSpell(*space.playable.iter().find(|p|p.card_id==self.spell).unwrap())
 }
 fn choose_attackers(&mut self,_:PlayerId,_:&[CardId],_:&[DefenderId])->Vec<(CardId,DefenderId)>{vec![]}
 fn choose_blockers(&mut self,_:PlayerId,_:&[CardId],_:&[CardId],_:Option<usize>)->Vec<(CardId,CardId)>{vec![]}
 fn choose_targets_for(&mut self, sa:&mut SpellAbility, game:&GameState, pools:&[ManaPool])->bool { manabrew_engine::spellability::choose_targets_by_kind(self,sa,game,pools) }
 fn choose_target_player(&mut self,_:PlayerId,v:&[PlayerId],_:Option<&SpellAbility>)->Option<PlayerId>{v.first().copied()}
 fn choose_target_card(&mut self,_:PlayerId,v:&[CardId],_:Option<&SpellAbility>)->Option<CardId>{v.first().copied()}
 fn choose_target_any(&mut self,_:PlayerId,_:&[PlayerId],_:&[CardId],_:Option<&SpellAbility>)->TargetChoice{TargetChoice::None}
 fn choose_land_or_spell(&mut self,_:PlayerId)->Option<bool>{None}
 fn choose_number(&mut self,_:PlayerId,_:Option<CardId>,_:&str,_:Option<&str>,_:i32,max:i32)->Option<i32>{Some(max)}
 fn pay_mana_cost(&mut self,_:PlayerId,_:CardId,_:&str,_:&str,_:&str,_:&str,_:bool,_:bool,_:&[CardId],_:&[ManaAbilityOption],_:&[CardId],_:&[CardId],_:&ManaPool)->ManaCostAction {
  let i=self.payment; self.payment+=1;
  if i<2 { ManaCostAction::TapForMana{card_id:self.helpers[i],mana_ability_index:Some(usize::MAX),express_choice:None} } else { ManaCostAction::Pay{auto:true} }
 }
}
#[test]
fn manual_convoke_then_failed_auto_payment() {
 std::thread::Builder::new().stack_size  (32*1024*1024).spawn(|| {
  let mut game=GameState::new(&["A","B"],40); let p=PlayerId(0);
  let mut add=|script:&str,zone| {let rules=forge_carddb::parse_card_script(script).unwrap(); let id=game.create_card(Card::from_rules(&rules,p)); game.move_card(id,zone,p); id};
  let spell=add("Name:Chord of Calling\nManaCost:X G G G\nTypes:Instant\nK:Convoke\nA:SP$ GainLife | Defined$ You | LifeAmount$ 1",ZoneType::Hand);
  let a=add("Name:Elvish Archdruid\nManaCost:1 G G\nTypes:Creature Elf Druid\nPT:2/2\nA:AB$ Mana | Cost$ T | Produced$ G | Amount$ X\nSVar:X:Count$Valid Creature.YouCtrl+Elf",ZoneType::Battlefield);
  let b=add("Name:Elvish Warmaster\nManaCost:1 G\nTypes:Creature Elf Warrior\nPT:2/2\nT:Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | ValidCard$ Creature.Elf+YouCtrl+Other | Execute$ TrigToken\nSVar:TrigToken:DB$ Token | TokenScript$ g_1_1_elf_warrior",ZoneType::Battlefield);
  add("Name:Forest\nTypes:Basic Land Forest\nA:AB$ Mana | Cost$ T | Produced$ G",ZoneType::Battlefield);
  for _ in 0..3 { add("Name:Swamp\nTypes:Basic Land Swamp\nA:AB$ Mana | Cost$ T | Produced$ B",ZoneType::Battlefield); }
  for _ in 0..12 { add("Name:Elf\nManaCost:G\nTypes:Creature Elf\nPT:1/1",ZoneType::Battlefield); }
  add("Name:Moseo, Veins New Dean\nManaCost:3 B\nTypes:Creature Vampire\nPT:2/2\nT:Mode$ Phase | Phase$ EndOfTurn | Execute$ TrigLife\nSVar:TrigLife:DB$ GainLife | Defined$ You | LifeAmount$ 1",ZoneType::Battlefield);
  game.card_mut(a).set_summoning_sick(false);
  let mut agents:Vec<Box<dyn PlayerAgent>>=vec![Box::new(Agent{spell,helpers:[a,b],action:false,payment:0}),Box::new(manabrew_engine::agent::PassAgent)];
  GameLoop::new(2).priority_round(&mut game,&mut agents,true);
  assert_eq!(game.card(spell).zone,ZoneType::Hand); assert!(!game.card(a).tapped); assert!(!game.card(b).tapped);
 }).unwrap().join().unwrap();
}
