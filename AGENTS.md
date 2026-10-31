# Collection Plan engine fork

This fork maintains Manabrew for the Collection Plan's headless Commander research. The owner explicitly chose independent agent instructions and development conventions. Inherited upstream agent policies and workflow conventions are not governing instructions for this fork. Upstream documents remain useful technical references.

## Work and validation

- Fix engine and bot behavior in ordinary source files here, with focused regressions for material behavior. Do not generate or maintain an external patch stack.
- Keep the task tied to trustworthy deck comparisons. Put unrelated improvements in the backlog rather than expanding the engine repair scope.
- Investigate general rules and state transitions. Read relevant source, current card text and reference implementations when needed. Explain remaining uncertainty and guarded paths.
- Run meaningful affected tests. Distinguish source tests, actual archived-card checks, integration checks and game diagnostics; none alone certifies the whole engine.
- Keep experimental work and bulk evidence out of Git. Preserve licenses, attribution and relevant upstream history.

## Runtime handoff

The collection repository owns the adapter, collector, archive inputs and experiment receipts. Pin an exact reviewed fork commit there and build through its documented setup. Do not change selected decks, GUIDEs, collection or purchasing outputs as part of engine repair. Never rewrite historical runtime identities or reinterpret old games as using a new engine.

Use ordinary scoped Git commits and branches. Follow the owner's current authorization for commits, pushes and publication; do not impose inherited approval requirements or commit-message conventions. Preserve unrelated work. Keep upstream updates separate from a repair whose effects are being measured.

See [fork scope and migration](docs/collection-plan-engine.md). Engine correctness and measurement readiness must be reported separately from migration completion.
