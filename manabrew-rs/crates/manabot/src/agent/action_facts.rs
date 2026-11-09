//! Facts supplied by the host for offered actions. Unknown is never a zero gain.
use serde::{Deserialize, Serialize};

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
