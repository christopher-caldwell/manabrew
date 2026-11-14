//! Decision strategies for a bot. The lifecycle in `BotState` is fixed; the
//! AI plugged into it is not.
//!
//! To add a new agent: define a struct implementing [`BotAgent`], add a
//! variant to [`AgentKind`], and wire it in [`AgentKind::build`]. The room
//! picks which agent to spawn via the `agent` field of the bot config.
//!
//! A hosted room may also answer its own bot seats in process rather than
//! over the relay, and it builds that responder from the same configured
//! agent (`self-hosted-node`'s `Config::bot_agent`). Anything that chooses an
//! agent has to set that one field; a second, separate choice would win over
//! the seat's own, because the relay's answer arrives later and is discarded
//! as a duplicate -- with no error, and a legal-looking game.

use manabrew_agent_interface::agent_impl::Responder;
use manabrew_agent_interface::game_view_dto::GameViewDto;
use manabrew_agent_interface::prompt::{
    AgentPrompt, ChooseActionOutput, ClientToServerMessage, PromptOutput,
};
use serde::{Deserialize, Serialize};

pub mod action_facts;
pub mod bias;
pub mod comparative;
pub use action_facts::{ActionFacts, EffectFact, PaymentFacts, ResourceFacts};
pub use bias::{ActionBias, BotSkip};
pub mod simple_ai;
pub use simple_ai::SimpleAi;

pub trait BotAgent: Send {
    fn observe(&mut self, _view: GameViewDto) {}
    fn observe_action_facts(&mut self, _facts: &[ActionFacts]) {}
    fn observe_resources(&mut self, _facts: Option<ResourceFacts>) {}
    fn observe_payments(&mut self, _facts: &[PaymentFacts]) {}
    fn take_decision_record(&mut self) -> Option<serde_json::Value> {
        None
    }
    fn decide(&mut self, prompt: AgentPrompt) -> Option<PromptOutput>;
}

/// Wire-level selector for which built-in agent the bot should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    #[default]
    Simple,
    Strategic,
    Consequence,
}

impl AgentKind {
    pub fn build(self) -> Box<dyn BotAgent + Send> {
        match self {
            AgentKind::Simple => Box::<SimpleAi>::default(),
            AgentKind::Strategic => Box::new(comparative::Comparative::new(
                comparative::Variant::Strategic,
            )),
            AgentKind::Consequence => Box::new(comparative::Comparative::new(
                comparative::Variant::Consequence,
            )),
        }
    }
}

pub struct BotResponder {
    agent: Box<dyn BotAgent + Send>,
}

impl BotResponder {
    pub fn new(agent: Box<dyn BotAgent + Send>) -> Self {
        Self { agent }
    }
}

impl Default for BotResponder {
    fn default() -> Self {
        Self::new(AgentKind::default().build())
    }
}

impl BotResponder {
    pub fn take_decision_record(&mut self) -> Option<serde_json::Value> {
        self.agent.take_decision_record()
    }

    pub fn observe_resources(&mut self, facts: Option<ResourceFacts>) {
        self.agent.observe_resources(facts);
    }

    pub fn observe_payments(&mut self, facts: &[PaymentFacts]) {
        self.agent.observe_payments(facts);
    }

    /// Feed host-derived facts for the current offered actions.
    pub fn observe_action_facts(&mut self, facts: &[ActionFacts]) {
        self.agent.observe_action_facts(facts);
    }

    /// Feed the seat's latest view to the agent, as a state envelope would.
    pub fn observe(&mut self, view: GameViewDto) {
        self.agent.observe(view);
    }
}

impl Responder for BotResponder {
    fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
        let prompt_id = prompt.prompt_id;
        let action = self
            .agent
            .decide(prompt)
            .unwrap_or(PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                until: None,
                exhaust_stack: false,
            }));
        ClientToServerMessage::Response { prompt_id, action }
    }
}
