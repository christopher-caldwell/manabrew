use manabrew_agent_interface::agent_impl::{PromptAgent, Responder};
use manabrew_agent_interface::prompt::{
    AgentPrompt, ChooseFromSelectionOutput, ClientToServerMessage, PromptInput, PromptOutput,
};
use manabrew_engine::agent::PlayerAgent;
use manabrew_engine::ids::PlayerId;

struct InvalidSelectionResponse;

impl Responder for InvalidSelectionResponse {
    fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
        assert!(matches!(prompt.input, PromptInput::ChooseFromSelection(_)));
        ClientToServerMessage::Response {
            prompt_id: prompt.prompt_id,
            action: PromptOutput::ChooseFromSelection(
                ChooseFromSelectionOutput::SelectionDecision {
                    chosen_indices: vec![1, 1, 99, 0],
                },
            ),
        }
    }
}

#[test]
fn choose_mode_discards_duplicate_and_out_of_range_response_indices() {
    let mut agent = PromptAgent::new(
        PlayerId(0),
        "selection validation regression".into(),
        InvalidSelectionResponse,
    );
    let descriptions = vec!["first".to_string(), "second".to_string()];

    assert_eq!(
        agent.choose_mode(PlayerId(0), &descriptions, 1, 2, None),
        vec![1, 0]
    );
}
