# Research notes

Source-backed investigations that inform Via design decisions. Research records evidence and tradeoffs;
it does not establish an accepted architecture, implemented behavior, or deployment state.

For the connected bridge and monitoring design, start with the permanent
[Via correctness and monitoring design-and-rationale record](../design/via-correctness-and-monitoring/README.md).
It connects the findings below to concrete situations, alternatives, recommendations, and accepted
boundaries. Topic notes retain detailed evidence; ADRs own accepted consequential decisions.

## Notes

- [Via contracts and a future re-fork](via-contracts-and-a-future-refork.md): the shared context for
  the nine current research questions, inspected zkSync integration interfaces, and evidence that
  remains useful when implementation files move. This is not a complete Via feature inventory.
- [Withdrawal authorization](withdrawal-authorization-and-refork.md): signer checks, exact proposals,
  expected obligations, and comparable bridge implementations.
- [Proof verification and historical evidence](proof-verification-and-history.md): verification
  inputs, result handling, and historical trust.
- [Coordinator request authentication](coordinator-request-authentication.md): signed content,
  request identity, retries, and signing-session conflicts.
- [Deposit compatibility evidence](deposit-compatibility-evidence.md): historical interpretation
  and rollout evidence without reopening accepted decoding policy.
- [Conflicting deposit messages](conflicting-deposit-messages.md): consistent handling of the
  accepted exclusive-carrier rule recorded in [ADR 0006](../adr/0006-reject-recognized-deposit-carrier-coexistence.md).
- [Historical upgrade reconstruction](historical-upgrade-reconstruction.md): approved bytes,
  version selection, and reconstruction evidence.
- [Inscription monitoring](inscription-monitoring-design.md): bounded observations, unavailable
  data, metrics, and operator ownership.
- [Synthetic bridge checks](synthetic-bridge-checks.md): end-to-end probes, durable progress,
  spending limits, and failure detection.
- [Independent monitoring failure detection](independent-monitoring-failure-detection.md): external
  heartbeat receivers and proof that a failed monitoring host produces an alert.
- [Withdrawal intent and observed Bitcoin payments](withdrawal-intent-and-observation.md): comparable
  storage models, observation-first ingestion, identity and replay rules, and candidate Via ownership.
  [ADR 0004](../adr/0004-separate-expected-withdrawals-from-observed-payments.md) accepts the storage
  separation. Schema, signing, hold, and recovery choices remain open.

These studies retain research status. Linked ADRs identify accepted subdecisions; the remaining
recommendations are not accepted architecture decisions.

## Research before and during design and implementation

Use this workflow when choosing an approach or resolving an unfamiliar implementation detail, both
before coding and as questions arise during implementation. Examples include data structures,
function and type names, control flow, design patterns, lifecycles, and protocol or stored-data meaning.
An initial research pass does not settle later choices. Mechanical edits that follow an established
contract can reuse its evidence without a new survey.

1. Read the existing research and ADRs for the topic. Reuse evidence whose revisions and assumptions
   still fit. State the current question and accepted constraints, then identify the Via owner,
   its callers, sibling implementations, and any candidate already addressing the problem.
2. Inspect the relevant inherited code and zkSync Era upstream implementation before designing a
   replacement. Pin the compared revisions. Distinguish the fork's baseline from a newer local
   checkout and from verified current upstream. A matching type or file name is not compatibility.
3. Find comparable projects, chains, or software that solve the specific problem. Inspect primary
   source code, official specifications, and relevant tests. Look for reusable design patterns,
   standards, RFCs or BIPs, data structures, naming conventions, control flow, and lifecycle behavior.
   Cite exact revisions and paths where available. Label moving documentation and inaccessible sources.
   Distinguish a published standard from a draft and an inspected test from an executed one.
4. Explain what transfers to Via and what does not. Compare trust assumptions, authorization,
   ordering, retries, restart, reorgs, and compatibility where relevant. Name the cost of an
   alternative rather than importing another project's design wholesale.
5. Update the existing topic note with source revisions, findings, alternatives, limits, and the
   recommended choice. Create a new note only for a distinct question. Record unresolved choices
   and the local proof needed before implementation or release.

Repeat the relevant steps when implementation exposes a new question, contradicts an assumption,
or needs a different approach. Research the specific uncertainty rather than restarting the whole survey.
Stop when the question is answered; there is no required number of comparison projects.

The research is sufficient for the current question when a reviewer can trace the recommendation to
primary evidence, understand why the closest alternatives fit or fail, and identify what remains unproven.
An unsuccessful search is a bounded finding, not proof that no precedent exists. If a relevant
source is inaccessible, state the gap instead of filling it with a model-generated claim.
Research does not accept a decision or prove a Via implementation correct.

When independent consultations are requested, complete the initial source investigation first.
Give each researcher the same open questions, accepted constraints, source revisions, findings,
and proposed choices. Ask each to inspect primary sources, find other relevant implementations,
and challenge the proposals without reading the other researchers' new conclusions. Describe this
as independent investigation informed by shared input, not a blind comparison. Verify material
claims and record disagreements before presenting the choices for acceptance.

Use the `technical-writing` skill for the write-up when available. Otherwise apply the same standard:
plain language, evidence separated from recommendations, and a source for each material claim.
Use the placement and disclosure rules below.

### Design-and-rationale records

Use this step to turn investigated questions into a lasting design-and-rationale document. Inputs
include the original questions, factual answers from completed research tickets, comparable software
and its real code, recommendations, and accepted choices. A resolved research ticket answers an
evidence question; it does not necessarily accept an architectural choice. The document explains
what we learned, what we propose, what we chose and why, and what remains to be decided.
Reuse the research already gathered; investigate further only where an important claim or
alternative lacks evidence.

The pending design decision record and the later implementation rationale are lifecycle states of
the same document. Keep answered questions and accepted reasoning as choices are settled; do not
replace them with a fresh implementation summary that loses why the design was selected.

1. Establish one reader-facing entry point. For one topic, improve its existing note. For a connected
   design, maintain a permanent record under `docs/design/`, linked to detailed research and ADRs.
   Keep disclosure-safe reasoning alongside the code in version-controlled documentation; do not
   reduce it to an external pointer. Keep restricted findings and required evidence in an approved
   private version-controlled store with a remote copy, referenced only where disclosure permits.
   Earlier full private captures remain historical evidence, not competing live design records.
   Scratch directories and Git exclusion alone do not provide durable storage or backup.
2. Begin each problem with a concrete situation: who acts, what information they have, what happens
   next, and what could go wrong. Define unfamiliar terms before using them. Phrase the question as
   a choice about behavior, not an unexplained label such as “authority” or “admission.”
3. Describe verified current behavior separately from the desired behavior. Name the responsible
   components and relevant symbols. Preserve important conditions, state changes, and failure
   transitions; a recommendation must not replace the observation that motivated it.
4. Explain how comparable software handles the same problem. Where code helps, include a short,
   verbatim excerpt from inspected primary source with repository, pinned revision, path, line range,
   and applicable attribution. Explain what the code does and which assumptions do not transfer.
   Keep each excerpt contiguous and mark omissions outside the code fence. Identify specification
   text, illustrative examples, or pseudocode as such; never present them as implementation source.
   If useful code cannot be verified or reproduced, state the limit and link the available evidence.
5. Present the viable alternatives, their observable consequences, and the recommendation with its
   reasons. Separate established facts, assumptions, and remaining evidence needs. Examples may use
   hypothetical values, but must not silently select fees, deadlines, networks, or operating policy.
6. Distinguish answered factual questions from pending design choices. Record each accepted choice
   and its reason, including rejected alternatives worth remembering. Link consequential architectural
   tradeoffs to an ADR; several related questions may support one ADR. Keep implementation intent,
   implemented behavior, and verified rollout distinct. Update the record as those states change,
   while keeping task ownership and execution in the tracker. A recommendation is not a commitment
   to implement it, and completed research is not proof of implementation.
7. Check that every original question is covered, excerpts match their pinned source, and references
   resolve. Read each explanation without the conversation: the situation, choice, tradeoffs, and
   reason for the recommendation must be understandable without a private shorthand vocabulary.

Use scenario-first prose as the main explanation; use question IDs only for traceability. A record
is ready for discussion when the reader can explain what each option changes and why another project
is relevant, not merely recognize its names. Work through one coherent decision at a time after that
explanation. Neither writing the record nor recommending an order accepts its architectural choices
or authorizes implementation.

### Learning and workflow improvement

Use this check for an evidenced correction, an inadequate instruction, or a reusable procedure shown
by independent tasks. Finishing ordinary research updates its topic note; it does not require a
separate improvement review. Repeated messages about one task are one occurrence. A single
demonstrated correctness or safety failure is enough to address within the authorized scope.

State the observed and expected result, then check the existing owner, skills, candidates, and prior
attempts. Before adding a rule, establish whether the relevant instruction was reached and ask:
**would following it correctly have prevented the result?** Match the intervention to that evidence:

| Gap | Existing owner and next action |
| --- | --- |
| Missing or wrong fact | Correct the topic note or implemented guide with its source; keep changes to accepted policy separate. |
| Correct instruction was not reached | Sharpen its conditional pointer or move essential context up a level. |
| Instruction was reached but reasonably misread | Clarify that wording and exercise the ambiguous case. |
| Tool, procedure, or runtime cannot deliver the required result | Address the mechanism under normal change discipline, not another warning. Record an out-of-scope capability gap in the tracker. |
| Independent tasks repeatedly reconstruct an unsupported procedure | Check existing skills first; propose a reusable procedure with an owner and observable trigger. Repetition alone does not authorize building it. |
| Human decision, permission, or required evidence is missing | Keep the dependent choice pending; this is not automatically a workflow defect. Continue independent authorized work and ask when the missing decision blocks required work. |

Keep actionable proposals and failed or abandoned attempts on the existing tracker item: evidence,
cause or hypothesis, owning file, intended change, observable acceptance criterion, and unresolved
risks. Amend factual evidence in its topic note and cross-topic conclusions in the design record;
accepted consequential decisions retain their ADR owner. A merged instruction change keeps its
durable rationale in that instruction and Git history, not another intervention log. Restricted
evidence stays private; public explanations describe system behavior.

Verify an authorized change against the relevant scenario and nearby behavior that must remain
unchanged. For wording or trigger changes, cover applicable cases, cases that must not trigger, and
fresh paraphrases, including misleading vocabulary or missing prerequisites where relevant. Choose
cases for distinct failure modes, not a fixed count. Link checks do not prove an agent followed a
pointer; interpretation probes are bounded evidence. If a claim cannot yet be tested, retain the
evidence gap and lower confidence. The next task encountering the scenario should record whether
the change held. Proposing an improvement does not expand the current task's authorization.

Source comparison, not an instruction to invoke another workflow: Centaur's
[learning synthesis](https://github.com/paradigmxyz/centaur/blob/adb8d2d14c512be368c95418b6e6f9e18b41c10f/.agents/skills/learning-synthesis/SKILL.md),
[improve-gap-task](https://github.com/paradigmxyz/centaur/blob/adb8d2d14c512be368c95418b6e6f9e18b41c10f/.agents/skills/improve-gap-task/SKILL.md),
and [intervention history](https://github.com/paradigmxyz/centaur/blob/adb8d2d14c512be368c95418b6e6f9e18b41c10f/.agents/skills/improve-gap-task/references/history.md)
inform these checks. Their Slack intake, nightly selection, JSON handoffs, and automatic PRs are not adopted.

### Principle skills for concrete implementation questions

When a question below arises, read the named skill if it is installed and apply it to that question.
These are selection cues, not a requirement to run every skill. They complement source research;
they do not replace it or override accepted Via contracts, compatibility requirements, or safety rules.

| Skill | When to use it |
| --- | --- |
| `principle-foundational-thinking` | Before choosing core types or data structures. Trace access patterns and concurrent ownership before writing the dependent logic. |
| `principle-model-the-domain` | When stateful logic accumulates booleans, repeated shape assumptions, or scattered transitions. Find the structure that represents the actual domain. |
| `principle-type-system-discipline` | When designing signatures or types. Distinguish semantic identities, exclude invalid combinations, and handle variants exhaustively. |
| `principle-boundary-discipline` | When placing parsing, validation, error handling, or framework adapters. Establish which boundary validates each fact and which internal invariants follow. |
| `principle-separate-before-serializing-shared-state` | When concurrent actors may mutate the same object. Separate independent facts first; serialize access when shared authority is necessary. |
| `principle-make-operations-idempotent` | When implementing commands, ingestion, or lifecycle transitions that can repeat after retries or crashes. Define reconciliation and conflicting-repeat behavior. |
| `principle-exhaust-the-design-space` | When research leaves several viable architectural approaches with no established fit. Compare distinct prototypes or sketches before choosing. |
| `principle-redesign-from-first-principles` | When a new requirement conflicts with the current design. Reconsider the affected design as a whole, then make a scoped, coherent change. |
| `principle-fix-root-causes` | When debugging a failure. Trace the observed symptom to its cause and verify the correction rather than hiding the symptom. |
| `principle-attack-the-premise` | When multiple fixes built on the same premise fail the same check. State the premise and collect evidence that can disprove it before another fix. |
| `principle-subtract-before-you-add` | When sequencing an addition or refactor. Identify obsolete code and redundant mechanisms before building on them. |
| `principle-laziness-protocol` | When a solution adds layers, signal propagation, or duplicated decisions. Look for a smaller design that still satisfies the full contract. |
| `principle-minimize-reader-load` | When code is hard to trace. Reduce unnecessary indirection and the mutable state a reader must track. |
| `principle-migrate-callers-then-delete-legacy-apis` | When replacing an internal API. Inventory and migrate its callers, then remove the obsolete path in the same change. |
| `principle-outcome-oriented-execution` | During a planned rewrite or migration with explicit verification boundaries. Reach the agreed end state without accumulating throwaway compatibility code. |
| `principle-test-behavior-not-implementation` | When adding, changing, or retaining a test. Assert observable results that fail for a plausible defect, not internal wiring. |
| `principle-encode-lessons-in-structure` | When the same correction recurs. Consider whether a type, shared helper, check, or lint can enforce the invariant. |
| `principle-guard-the-context-window` | During large investigations. Keep focused evidence and summaries available without flooding the working context with raw output. |

Apply each principle within the evidence boundary. A typed database row is not proof of current chain
validity. Idempotent delivery does not permit MuSig2 secret-nonce reuse. Simplification does not justify
removing required checks, and redesign does not authorize a protocol change or a live migration.
If a skill is unavailable, use its stated purpose without claiming to have read or executed it.

## Where information belongs

| Material | Location |
| --- | --- |
| Cross-topic design explanation, alternatives, recommendations, and current decision status | `docs/design/`, linking research and accepted ADRs |
| Primary-source findings, alternatives, limitations, and open design questions | `docs/research/` |
| Accepted, consequential architectural decisions and their rationale | `docs/adr/` |
| Established domain vocabulary | `CONTEXT.md` |
| Implemented behavior and operator/developer instructions | `docs/via_guides/` |
| Work sequencing, ownership, and unresolved decision tracking | The planning tracker |
| Restricted findings and required supporting evidence | An approved private version-controlled store with a remote copy |
| Raw transcripts, temporary experiments, and incidental execution metadata | Excluded local working files; retain required evidence in its durable owner |

A research note should state its question, status, source revisions, findings, limits, and open questions.
Cite primary sources at immutable revisions where possible. Distinguish observed source behavior from
recommendations and label unverified assumptions. Keep tool transcripts and incidental execution status
out of the durable note.

When a consequential choice is accepted, record the decision in an ADR when its tradeoffs warrant one.
Link the ADR to the research rather than copying the evidence; link the research back to the decision.
Do not turn an unresolved recommendation into an accepted ADR. A research note is not a second task list.

When moving a document's authority, update the old live entry point and its callers as well as the
new one. An immutable historical capture and a still-maintained branch are different objects.
Keep the handoff explicitly pending until the ownership notices agree; a new pointer alone does
not retire the old authority.

[Proposed ADR 0005](../adr/0005-preserve-documentation-ownership-and-provenance.md) examines documentation
ownership and upstream provenance across a future re-fork. It does not change the policy above.

Before committing or publishing a note, check it for credentials, private tracker content, and findings
that require disclosure approval. Adding a local document does not authorize publication.
