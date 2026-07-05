export const meta = {
  name: 'test-suite',
  description: 'Full multi-stack Chimera test pass: Rust, Python, contracts, guardrails in parallel lanes, then per-failure triage',
  whenToUse: 'Run after significant changes or before pushing. Lanes degrade gracefully (SKIPPED) when a toolchain such as forge is absent. Optionally pass args as a lane list, e.g. ["rust","python"], to run a subset.',
  phases: [
    { title: 'Test', detail: 'parallel lane runners: rust, python, contracts, guardrails' },
    { title: 'Triage', detail: 'one diagnostician per failure, spawned as soon as its lane finishes' },
  ],
}

// Each lane's authoritative instructions live in .claude/agents/<runner>.md —
// the lane agent reads that file first, so routine content has one source of
// truth and stays editable without touching this script.
const LANES = [
  { key: 'rust', runner: 'rust-test-runner' },
  { key: 'python', runner: 'python-test-runner' },
  { key: 'contracts', runner: 'contract-test-runner' },
  { key: 'guardrails', runner: 'guardrail-checker' },
]

const LANE_SCHEMA = {
  type: 'object',
  required: ['lane', 'steps'],
  properties: {
    lane: { type: 'string' },
    steps: {
      type: 'array',
      items: {
        type: 'object',
        required: ['name', 'command', 'status', 'summary'],
        properties: {
          name: { type: 'string' },
          command: { type: 'string' },
          status: { type: 'string', enum: ['passed', 'failed', 'skipped', 'warn'] },
          summary: { type: 'string', description: 'counts and one-line outcome' },
          failures: {
            type: 'array',
            items: {
              type: 'object',
              required: ['test', 'excerpt'],
              properties: {
                test: { type: 'string' },
                excerpt: { type: 'string', description: 'assertion/panic/revert excerpt, trimmed' },
              },
            },
          },
        },
      },
    },
  },
}

const TRIAGE_SCHEMA = {
  type: 'object',
  required: ['test', 'classification', 'root_cause', 'suggested_minimal_fix', 'safety_relevant'],
  properties: {
    test: { type: 'string' },
    classification: { type: 'string', enum: ['product-bug', 'test-bug', 'env-issue', 'flaky'] },
    root_cause: { type: 'string', description: 'file:line plus one-sentence cause' },
    evidence: { type: 'string' },
    suggested_minimal_fix: { type: 'string' },
    blast_radius: { type: 'string' },
    safety_relevant: { type: 'boolean', description: 'true if the funds path (executor/pacing/sweep) is implicated' },
  },
}

const requested = Array.isArray(args) ? args : args && Array.isArray(args.lanes) ? args.lanes : null
const lanes = requested ? LANES.filter((l) => requested.includes(l.key)) : LANES
if (requested && lanes.length < requested.length) {
  log(`unknown lane(s) ignored: ${requested.filter((k) => !LANES.some((l) => l.key === k)).join(', ')}`)
}
log(`running lanes: ${lanes.map((l) => l.key).join(', ')}`)

function lanePrompt(lane) {
  return [
    `You are running the "${lane.key}" lane of Project Chimera's test suite from the repo root.`,
    `First Read .claude/agents/${lane.runner}.md and execute that routine exactly as written — it is the authoritative instruction set for this lane.`,
    'Run every step even if an earlier one fails, so the report is complete.',
    'Observe only: never edit source, tests, fixtures, or config, and never touch live RPCs or broadcast anything.',
    `Return the lane report with lane="${lane.key}" and one entry per step.`,
  ].join('\n')
}

function triagePrompt(laneKey, failure) {
  return [
    `A check in Project Chimera's "${laneKey}" test lane failed. Diagnose it — do not fix it.`,
    'First Read .claude/agents/test-triage.md and follow that method exactly.',
    `Failing step: ${failure.step}`,
    `Failing test/check: ${failure.test}`,
    `Error excerpt:\n${failure.excerpt}`,
  ].join('\n')
}

// pipeline(): each lane's failures go to triage the moment that lane
// finishes — fast lanes (guardrails, python) triage while rust compiles.
const results = await pipeline(
  lanes,
  (lane) =>
    agent(lanePrompt(lane), {
      label: `lane:${lane.key}`,
      phase: 'Test',
      schema: LANE_SCHEMA,
    }),
  (laneResult, lane) => {
    if (!laneResult) return { lane: { lane: lane.key, steps: [], error: 'lane agent failed' }, triage: [] }
    const failures = laneResult.steps.flatMap((s) =>
      s.status !== 'failed'
        ? []
        : s.failures && s.failures.length
          ? s.failures.map((f) => ({ step: s.name, ...f }))
          : [{ step: s.name, test: s.name, excerpt: s.summary }],
    )
    if (!failures.length) return { lane: laneResult, triage: [] }
    log(`${lane.key}: ${failures.length} failure(s) → triage`)
    return parallel(
      failures.map((f) => () =>
        agent(triagePrompt(lane.key, f), {
          label: `triage:${f.test.slice(0, 40)}`,
          phase: 'Triage',
          schema: TRIAGE_SCHEMA,
        }),
      ),
    ).then((triage) => ({ lane: laneResult, triage: triage.filter(Boolean) }))
  },
)

const report = results.filter(Boolean)
const totals = { passed: 0, failed: 0, skipped: 0, warn: 0 }
for (const r of report) for (const s of r.lane.steps || []) totals[s.status] = (totals[s.status] || 0) + 1
log(`done: ${totals.passed} passed, ${totals.failed} failed, ${totals.skipped} skipped, ${totals.warn} warn`)
return { totals, lanes: report }
