use manabrew_agent_interface::game_view_dto::GameViewDto;
use manabrew_agent_interface::prompt::AvailableAction;

/// Optional adjustment to SimpleAi's own action scores. One value per offered action, in order.
pub trait ActionBias: Send {
    fn adjust(
        &mut self,
        view: Option<&GameViewDto>,
        player_id: &str,
        actions: &[AvailableAction],
        base: &[i32],
    ) -> Vec<i32>;
}
