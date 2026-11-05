use manabrew_agent_interface::agent_impl::{PromptAgent, Responder};
use manabrew_agent_interface::prompt::{AgentPrompt, ClientToServerMessage};
use manabrew_engine::agent::PlayerAgent;
use manabrew_engine::ids::PlayerId;

struct NoPrompt;

impl Responder for NoPrompt {
    fn respond(&mut self, _prompt: AgentPrompt) -> ClientToServerMessage {
        panic!("a forced color must be resolved without prompting");
    }
}

#[test]
fn choose_color_resolves_a_single_legal_color_without_prompting() {
    let mut agent = PromptAgent::new(PlayerId(0), "regression".into(), NoPrompt);
    let valid_colors = vec!["White".to_string()];

    assert_eq!(
        agent.choose_color(PlayerId(0), &valid_colors),
        Some("White".to_string())
    );
}
