# Collection Plan engine fork

This fork maintains the Manabrew engine and bot used by the Collection Plan's
headless Commander research. Engine changes and their regression tests belong in
ordinary source files here. The collection repository retains its adapter,
collector, immutable runtime receipts, experiment design and analysis tools.
The owner explicitly replaced upstream agent instructions and conventions with
this fork's own root guidance.

Upstream: <https://github.com/witchesofthehill/manabrew>.
Fork: <https://github.com/christopher-caldwell/manabrew>.
The import starts at upstream commit
`64fea8998a08c8b193ac3bc79478eaf57fd45b77`, without updating upstream behavior at
the same time. The working branch is `collection-plan-engine`.

## Initial source import

The initial working tree imports the Collection Plan's 46 production patches and
19 regression fixture patches through `0046-convoke-mixed-cancellation.patch`.
Tests stay in the source tree rather than being applied before tests and removed
before release builds. Preserve the upstream license files and attribution.

With the test fixtures temporarily reversed, all 135 changed production files
match the source used by the isolated DEV-027 validation build byte for byte.
That build produced runtime
`f3c5cef3c7b50e937d506cf355f2a05a891e03aca769fc63e53f7d5799e35891`.
This is source-equivalence evidence, not a new build of this checkout or a claim
that the engine is fully correct.

The imported repair scope includes mana ability identity, no-progress handling,
departure and counter LKI, protection duration, damage source attribution,
controller-list consistency and bounded convoke cancellation. DEV-027 remains
scoped: complex mixed effects still reach explicit guards. Nykthos's broader
costed mana planning and cancellation repair is integrated as a separate source
change after the initial import. Its source tests and archived-card checks passed
in the repair workspace; the actual fork build still requires final setup gates.
Do not launch the planned measurement study on the strength of this migration.

The Nykthos repair covers its ChooseColor-to-Mana classification, net available
mana with basic-land and Sol Ring funding, restricted mana, nested cancellation
after a draw or self-sacrifice, and retry after real progress. Complex triggered
and replacement paths remain explicitly guarded or unverified. The feasibility
planner can understate availability in unsupported positions. The curated
repair-source archive receipt passed 226 checks, and its source suites passed
389 engine, 29 bot and two interface tests. These are scoped observations, not
universal engine certification or permission for bulk measurement.

## Runtime and change discipline

Commit reviewed engine fixes and tests here, then pin the full fork commit in the
collection adapter's `sim/manabrew/engine-source.json`. A branch name or a dirty
working tree cannot identify a measurement runtime. Build through the adapter's
documented setup and actual-archive gates, preserving the exact archive and
runtime receipts. The adapter also needs its own code and source checks to pass.

Historical patch-built runtime objects, old manifests, traces and test receipts
keep their existing identities. Never rewrite them to claim they used this fork.
Keep new measurement separate from diagnostics and use a fresh runtime identity.

Bring upstream changes onto a separate branch and validate their effect before
updating the measurement pin. Do not combine an upstream refresh with a repair
whose effect is under investigation. This fork's purpose is trustworthy scoped
deck research, not general completion of the engine, desktop app or web service.

Local reconstruction and source-equivalence receipts are ignored data in the
collection checkout under `sim/.local/fork-adoption-20261031/`.

## Targeted candidate readiness repairs

The subsequent readiness audit reproduced four actual-list defects: zero damage
from paid Crackle with Power, unrelated effects suppressing Nykthos, mixed
convoke/painland cancellation, and infect bypassing damage prevention. Their
scoped repairs pass 393 engine library tests and 249 standalone archived-card
probes, including Chord with the three frozen opponent commanders together.
These source checks precede the combined published runtime build. Relevant or
unknown damage/mana triggers, other replacement effects and complex mixed cost
paths remain guarded; the recorded scope is not complete engine certification.
