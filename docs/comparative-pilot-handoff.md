# Comparative pilot source handoff

The engine source is the Git commit containing this file. Obtain its exact commit and tree with `git rev-parse HEAD HEAD^{tree}`. The Build controller's registered Implementation and PASS Audit, when issued, identify the reviewed dependency for the separate adapter Build. The adapter Build must pin that exact commit and must not silently change this checkout. This source handoff does not install or promote a runtime.

## Contract

`AgentKind::Simple` is the default control. `Strategic` and `Consequence` are selectable through `AgentKind::build` and `BotResponder`. The control ignores the additional fact channels. The strategic policy uses a separate resource-sensitive score. The consequence policy uses one `ProjectedPosition::value` for pass and each supported action. Its board, access, mana and exposure components are coarse heuristics, not a claim of playing strength.

Before each decision the host supplies a fresh public `GameViewDto`, action facts, resource totals and verified payments. `ActionFacts` describes the offered action's mode, cost, scope, conditional effect, unknowns and resolution uncertainty. `PaymentFacts` attests legal payment, including colors and additional costs, and reports mana spent and immediately remaining. `ResourceFacts` separates immediately available mana from future capacity. The host must mark unrepresentable continuous effects, replacements and other conditions unknown; the policy cannot certify host facts. The channels are consumed on decision and must be supplied again. Records are consumed with `take_decision_record` and describe the current output, evaluated alternatives and any fallback reason.

The evaluated subset is an active player's main phase with an empty stack, complete visible hand and battlefield, unambiguous action identities, normal casts from hand, fixed generic or single-color mana costs, and host-verified payment. Supported effects are creature development, controller draw within the library count, and a noncreature permanent producing one specified mana color on future turns. Zero-toughness creatures add no board pressure. Wipes, creature mana sources, unsupported modes or effects, missing facts, unrepresentable costs and inconsistent payment fall back to `SimpleAi`; unknown is not treated as zero gain. Mana abilities and undo-mana actions are skipped by the evaluator. Conditional projections assume the instruction resolves unchanged and are not guaranteed outcomes. A single unsupported offered action can cause fallback for the whole prompt, so fallback frequency matters when interpreting comparisons.

The pilot reads observations and facts without speculative engine execution. Engine effect RNG is thread-local outside cloned game state, so safe speculative branching and random-stream isolation remain unresolved.

## Evidence and remaining work

The author-designed fixture comparison reported control membership of 5/6 development and 6/7 withheld, and both challengers 6/6 and 7/7, including fallback. Only `protected_board` improved among withheld families. Neither challenger cleared the frozen two-family adoption rule. These fixtures are engineering screens, not independent expert judgments or proof of human-level play. No challenger needs to win or become the default.

The source gate uses the existing checkout and designated shared target, sequentially:

```sh
CARGO_TARGET_DIR=/Users/christophercaldwell/Code/projects/mtg/simon-data/runtimes/target cargo test --manifest-path Cargo.toml -p manabrew-engine -p manabrew-agent-interface -p manabot --lib --no-default-features
```

On 2026-11-09, this command passed in the existing checkout with Rust and Cargo 1.94.0 and the existing shared target. Manabot ran 149 tests, agent-interface 3, and engine 432. All passed; none were ignored or filtered. The engine test build emitted warnings. The comparative tests exercise production selection, current records and fallback, stale context, public-state changes, name and seat changes, menu order, uncertain wipes, costs, draw limits and resource accounting. These are source-level fixtures, not a native adapter or game-strength test.

The later adapter Build owns native bridge compilation at the reviewed engine commit, fact production, actual archived-card checks, candidate comparison and runtime acceptance. Source tests alone establish none of those, nor a fresh installation, general gameplay strength, or a repaired Sauron win share. Stage C and larger research campaigns remain held. Historical runtime identities and saved games retain their original meaning.
