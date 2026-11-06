use std::{cell::RefCell, rc::Rc};

use super::*;

/// Route native prompts to the agent controlling this player's decisions.
/// The PromptAgent still belongs to the acting player: legal actions, costs,
/// prompt IDs and its filtered view must never be rebuilt as the controller.
/// Observations/logs remain with their original observer to avoid duplicates.
pub struct TurnControlResponder<R: Responder> {
    responders: Rc<RefCell<Vec<R>>>,
    player: PlayerId,
    decider: PlayerId,
    state: Option<AgentMessage>,
}

impl<R: Responder> TurnControlResponder<R> {
    pub fn new(player: PlayerId, responders: Rc<RefCell<Vec<R>>>) -> Self {
        assert!(player.index() < responders.borrow().len());
        Self {
            responders,
            player,
            decider: player,
            state: None,
        }
    }
}

impl<R: Responder> Responder for TurnControlResponder<R> {
    fn observe_engine(&mut self, game: &GameState, pools: &[ManaPool]) {
        // The engine's current turn-control state identifies this decision's
        // controller. Never replace the acting seat used to construct prompts.
        self.decider = game
            .player(self.player)
            .controlled_by
            .filter(|p| p.index() < self.responders.borrow().len() && game.player(*p).is_alive())
            .unwrap_or(self.player);
        self.responders.borrow_mut()[self.player.index()].observe_engine(game, pools);
    }

    fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
        let mut responders = self.responders.borrow_mut();
        let responder = &mut responders[self.decider.index()];
        responder.decision_player(self.player);
        // Another player may have used this responder since our snapshot.
        // Always restore this decision's acting-player perspective.
        if let Some(state) = &self.state {
            responder.present(state);
        }
        responder.respond(prompt)
    }

    fn present(&mut self, message: &AgentMessage) {
        if matches!(message, AgentMessage::State(_)) {
            self.state = Some(message.clone());
        } else {
            let mut responders = self.responders.borrow_mut();
            let responder = &mut responders[self.decider.index()];
            responder.decision_player(self.player);
            if let Some(state) = &self.state {
                responder.present(state);
            }
            responder.present(message);
        }
    }

    fn await_ack(&mut self) -> ClientToServerMessage {
        self.responders.borrow_mut()[self.decider.index()].await_ack()
    }
    fn notify_engine(&mut self, event: &GameNotification) {
        self.responders.borrow_mut()[self.player.index()].notify_engine(event);
    }
    fn send_log(&mut self, entry: GameLogEntryDto) {
        self.responders.borrow_mut()[self.player.index()].send_log(entry);
    }
    fn send_snapshot(&mut self, snapshot: GameSnapshotEventDto) {
        self.responders.borrow_mut()[self.player.index()].send_snapshot(snapshot);
    }
}
