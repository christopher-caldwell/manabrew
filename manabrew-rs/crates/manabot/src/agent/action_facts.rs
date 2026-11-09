//! Facts supplied by the host for offered actions. Unknown is never a zero gain.
//! Facts are trusted host attestations for one fresh public observation and prompt.
//! The host must mark effects unknown when continuous effects, replacements, or
//! other conditions make the described conditional result unrepresentable.
//! The policy does not discover those rules from card text or certify the host.
//! All channels must be supplied again after each observation and decision.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceFacts {
    pub available: i32,
    pub capacity: i32,
}

/// Host-verified payment for an offered action in the current observation.
/// `remaining` is immediately usable mana after paying, not future capacity.
/// The host must establish legal payment, including colors and all additional costs,
/// without executing the action. The pilot validates the arithmetic and cost shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentFacts {
    pub action_id: String,
    pub spent: i32,
    pub remaining: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionFacts {
    pub action_id: String,
    pub mode: String,
    pub mana_cost: Option<String>,
    pub target_scope: Option<String>,
    pub effect: Option<EffectFact>,
    pub unknown: Vec<String>,
    /// Resolution can be countered, replaced, prevented or otherwise changed.
    pub resolution_uncertain: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectFact {
    Creature {
        power: i32,
        toughness: i32,
    },
    Draw {
        cards: i32,
    },
    DestroyCreatures,
    ManaPermanent {
        produced: Vec<String>,
        creature: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{BotAgent, SimpleAi};
    use manabrew_protocol::transport::AgentPrompt;
    #[test]
    fn control_ignores_facts_without_changing_its_answer() {
        let prompt: AgentPrompt = serde_json::from_value(serde_json::json!({
            "promptId": 1, "decidingPlayerId":"player-0",
            "input":{"type":"chooseAction","actions":[]}
        }))
        .unwrap();
        let mut control = SimpleAi::default();
        let mut with_facts = SimpleAi::default();
        with_facts.observe_action_facts(&[ActionFacts {
            action_id: "a".into(),
            mode: "normal".into(),
            mana_cost: Some("{1}".into()),
            target_scope: Some("controller".into()),
            effect: Some(EffectFact::Draw { cards: 2 }),
            unknown: vec![],
            resolution_uncertain: true,
        }]);
        assert_eq!(
            serde_json::to_value(control.decide(prompt.clone())).unwrap(),
            serde_json::to_value(with_facts.decide(prompt)).unwrap()
        );
    }
}
