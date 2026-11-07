use manabrew_engine::{
    agent::{PlayerAgent, PriorityActionSpace, TargetChoice},
    combat::DefenderId,
    game::GameState,
    game_loop::GameLoop,
    ids::{CardId, PlayerId},
    mana::ManaPool,
    player::actions::PlayerAction,
    spellability::SpellAbility,
};
use std::{cell::RefCell, rc::Rc};

struct CountingAgent(Rc<RefCell<Vec<PlayerId>>>);
impl PlayerAgent for CountingAgent {
    fn choose_targets_for(
        &mut self,
        _sa: &mut SpellAbility,
        _game: &GameState,
        _mana_pools: &[ManaPool],
    ) -> bool {
        true
    }

    fn mulligan_decision(
        &mut self,
        _player: PlayerId,
        _hand: &[CardId],
        _mulligan_count: u32,
    ) -> bool {
        true
    }

    fn choose_action(
        &mut self,
        _player: PlayerId,
        _action_space: Option<&PriorityActionSpace>,
        _request_action_space: &mut dyn FnMut() -> PriorityActionSpace,
    ) -> PlayerAction {
        self.0.borrow_mut().push(_player);
        PlayerAction::PassPriority
    }

    fn choose_attackers(
        &mut self,
        _player: PlayerId,
        _available: &[CardId],
        _possible_defenders: &[DefenderId],
    ) -> Vec<(CardId, DefenderId)> {
        Vec::new() // no attackers
    }

    fn choose_blockers(
        &mut self,
        _player: PlayerId,
        _attackers: &[CardId],
        _available_blockers: &[CardId],
        _max_blockers: Option<usize>,
    ) -> Vec<(CardId, CardId)> {
        Vec::new() // no blockers
    }

    fn choose_target_player(
        &mut self,
        _player: PlayerId,
        valid: &[PlayerId],
        _sa: Option<&SpellAbility>,
    ) -> Option<PlayerId> {
        valid.first().copied()
    }

    fn choose_target_card(
        &mut self,
        _player: PlayerId,
        valid: &[CardId],
        _sa: Option<&SpellAbility>,
    ) -> Option<CardId> {
        valid.first().copied()
    }

    fn choose_target_any(
        &mut self,
        _player: PlayerId,
        valid_players: &[PlayerId],
        valid_cards: &[CardId],
        _sa: Option<&SpellAbility>,
    ) -> TargetChoice {
        if let Some(&pid) = valid_players.first() {
            TargetChoice::Player(pid)
        } else if let Some(&cid) = valid_cards.first() {
            TargetChoice::Card(cid)
        } else {
            TargetChoice::None
        }
    }

    fn choose_sacrifice(
        &mut self,
        _player: PlayerId,
        valid: &[CardId],
        _source: Option<CardId>,
    ) -> Option<CardId> {
        valid.first().copied()
    }

    fn choose_land_or_spell(&mut self, _player: PlayerId) -> Option<bool> {
        None
    }
}
#[test]
fn priority_passes_once_per_surviving_player() {
    let mut game = GameState::new(&["A", "B", "C", "D"], 40);
    game.player_mut(PlayerId(1)).has_lost = true;
    game.player_mut(PlayerId(2)).has_lost = true;
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut agents: Vec<Box<dyn PlayerAgent>> = (0..4)
        .map(|_| Box::new(CountingAgent(calls.clone())) as Box<dyn PlayerAgent>)
        .collect();
    let mut driver = GameLoop::new(4);
    driver.set_provide_priority_action_space(false);
    driver.priority_round(&mut game, &mut agents, false);
    assert_eq!(*calls.borrow(), vec![PlayerId(0), PlayerId(3)]);
}
