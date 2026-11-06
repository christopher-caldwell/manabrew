use manabrew_agent_interface::agent_impl::{PromptAgent, Responder};
use manabrew_agent_interface::prompt::{
    AgentPrompt, ChooseFromSelectionOutput, ClientToServerMessage, PromptInput, PromptOutput,
};
use manabrew_engine::agent::PlayerAgent;
use manabrew_engine::ids::PlayerId;

struct AcceptFeasibleSelection;

impl Responder for AcceptFeasibleSelection {
    fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
        let PromptInput::ChooseFromSelection(input) = prompt.input else {
            panic!("choose_mode must produce a selection prompt");
        };
        assert_eq!(input.min_total, 2);
        assert_eq!(input.max_total, 2);
        assert_eq!(input.options.len(), 2);
        assert!(input
            .options
            .iter()
            .all(|option| option.weight == 1 && !option.can_repeat));

        ClientToServerMessage::Response {
            prompt_id: prompt.prompt_id,
            action: PromptOutput::ChooseFromSelection(
                ChooseFromSelectionOutput::SelectionDecision {
                    chosen_indices: vec![0, 1],
                },
            ),
        }
    }
}

#[test]
fn choose_mode_clamps_minimum_to_distinct_available_options() {
    let mut agent = PromptAgent::new(
        PlayerId(0),
        "native regression".into(),
        AcceptFeasibleSelection,
    );
    let descriptions = vec!["mode one".to_string(), "mode two".to_string()];

    assert_eq!(
        agent.choose_mode(PlayerId(0), &descriptions, 3, 2, None),
        vec![0, 1]
    );
}
