use manabrew_agent_interface::agent_impl::{PromptAgent, Responder};
use manabrew_agent_interface::prompt::{AgentPrompt, ClientToServerMessage, ChooseFromSelectionOutput, PromptInput, PromptOutput};
use manabrew_engine::agent::PlayerAgent;
use manabrew_engine::ids::PlayerId;

struct Capture { expected: (usize, usize, usize), seen: usize }
impl Responder for Capture {
    fn respond(&mut self, prompt: AgentPrompt) -> ClientToServerMessage {
        let PromptInput::ChooseFromSelection(input) = prompt.input else { panic!("wrong production prompt"); };
        assert_eq!((input.min_total, input.max_total, input.options.len()), self.expected, "actual choose_mode prompt has infeasible or changed bounds");
        assert!(input.options.iter().all(|option| option.weight == 1 && !option.can_repeat));
        self.seen += 1;
        ClientToServerMessage::Response { prompt_id: prompt.prompt_id, action: PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision { chosen_indices: (0..input.min_total).collect() }) }
    }
}

#[test]
fn selection_prompts_match_available_modes() {
    // Original cataloged case: min 3 / max 2 with only two available modes.
    // Neighboring controls cover excess maximum, no options, optional and valid exact selections.
    for (count, min, max, expected_min, expected_max) in [(2,3,2,2,2), (2,1,4,1,2), (0,1,1,0,0), (2,0,2,0,2), (2,1,1,1,1)] {
        let mut agent = PromptAgent::new(PlayerId(0), "regression".into(), Capture { expected: (expected_min,expected_max,count), seen: 0 });
        let descriptions = (0..count).map(|i| format!("mode {i}")).collect::<Vec<_>>();
        let chosen = agent.choose_mode(PlayerId(0), &descriptions, min, max, None);
        assert_eq!(agent.responder.seen, 1);
        assert_eq!(chosen, (0..expected_min).collect::<Vec<_>>());
    }
}
