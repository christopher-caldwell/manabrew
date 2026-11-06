use manabrew_engine::{game::GameState, ids::{CardId, PlayerId}, svar::resolve_count_svar};

#[test]
fn revolt_count_selects_controller_condition() {
    let mut game = GameState::new(&["A", "B"], 40);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    let source = CardId(0);
    let expression = "Count$Revolt.4.2";

    let absent = resolve_count_svar(expression, &game, source, controller);
    game.player_mut(opponent).permanents_left_battlefield_this_turn = 1;
    let opponent_only = resolve_count_svar(expression, &game, source, controller);
    game.player_mut(controller).permanents_left_battlefield_this_turn = 1;
    let present = resolve_count_svar(expression, &game, source, controller);
    game.player_mut(controller).permanents_left_battlefield_this_turn = 0;
    let reset = resolve_count_svar(expression, &game, source, controller);
    assert_eq!((absent, opponent_only, present, reset), (2, 2, 4, 2));
}
