use manabrew_agent_interface::game_view_dto::GameViewDto;
use manabrew_agent_interface::prompt::AvailableAction;

/// Why SimpleAi will not take an offered action at this prompt, whatever its score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotSkip {
    /// A mana ability or undoing mana: payment, not a play.
    Mana,
    /// Already chosen earlier in this priority window (for example, a cast the engine could not start).
    Tried,
    /// An activation the bot judges to do nothing useful now.
    Wasted,
    /// A cast that fails the bot's sanity check, such as one it estimates it cannot afford.
    Unsafe,
    /// Spending that would dip into the mana the bot's turn plan keeps open.
    Reserve,
}

impl BotSkip {
    pub fn as_str(self) -> &'static str {
        match self {
            BotSkip::Mana => "mana",
            BotSkip::Tried => "tried",
            BotSkip::Wasted => "wasted",
            BotSkip::Unsafe => "unsafe",
            BotSkip::Reserve => "reserve",
        }
    }
}

/// Optional adjustment to SimpleAi's own action scores. One value per offered action, in order.
///
/// `skips` says, per offered action, why the bot would not take it at all (`None` when it may).
/// The bot's own answer is the highest positive `base` among actions with no skip, the last one on
/// a tie, or a pass when there is none; the adjusted answer uses `base + adjustment` the same way.
pub trait ActionBias: Send {
    fn adjust(
        &mut self,
        view: Option<&GameViewDto>,
        player_id: &str,
        actions: &[AvailableAction],
        base: &[i32],
        skips: &[Option<BotSkip>],
    ) -> Vec<i32>;
}
