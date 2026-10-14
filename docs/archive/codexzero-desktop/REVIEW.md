# CodexZero Desktop preservation and review

Reviewed and preserved on October 14, 2026.

## Preservation status

This branch preserves the unfinished `oa-codexzero-desktop` experiment from
`/Users/tyceherrman/GitHub/forks/code-worktrees/oa-codexzero-desktop`.
Its base is upstream Codex commit `1ae2b9880e` (July 29, 2026). The snapshot
includes all 23 modified tracked files and all six untracked source files,
without rewriting or porting the implementation. All 29 were byte-compared
with the original worktree before publication.

The four ignored local setup files are preserved under `local-setup/`:
`.codex/config.toml`, `user.bazelrc`, `.serena/.gitignore`, and
`.serena/project.yml`. They contain a network domain allowlist, Bazel batch
mode, and Serena configuration, rather than implementation changes. They
are reference copies, not automatically activated settings; the Serena
project identity should be regenerated for a new checkout. No Serena memories
or caches were present in the inspected directory.

This is an archival snapshot, not a release or a validated implementation.
No build, runtime benchmark, or task-accuracy comparison was performed during
this review or preservation. Existing tests are preserved but were not run.
The upstream Rust changes are intentionally retained in `codex-rs` because
that is where the original experiment lived; this is not a new implementation
in the Code fork's `code-rs` workspace.

To recover the experiment independently of the original local clone:

```sh
git clone --single-branch --branch archive/codexzero-desktop-2026-10-14 https://github.com/TyceHerrman/code.git code-codexzero-archive
```

## Revised conclusion

Four of the five ideas still deserve investigation. The implementation is
stale, but that does not mean the problems it targets have been solved. The
prior recommendation to retire the experiment was premature.

The review checked live repository heads: `just-every/code` at
`41f66ca5d6193b32703333923c8158af12c5657a` (September 29, v0.6.196), and
upstream Codex at `1ffa95ee8a02ec68f6ccd758998bcb7d4b32bcdb` (October 14).
It traced output capture, truncation, serialization, duplicate handling,
terminal waits, and upstream's newer Code Mode path, then checked related
issues and recent reports.

| Feature | Revised assessment | Priority |
| --- | --- | --- |
| Event-driven waiting | Strong evidence of a remaining problem | Highest |
| Command-aware summaries | Still valuable; existing truncation is a different mechanism | High |
| Exact duplicate results | Worth a controlled evaluation; previously dismissed too quickly | Medium |
| Lossless terminal codec | Worth measuring on real logs; narrower opportunity | Medium |
| Compact execution metadata | Partly redundant today; investigate as a supporting change | Low |

## Event-driven waiting

This has the strongest case. Both repositories already use notifications
internally, but their terminal collection loops still return at deadlines.
Efficiently waiting inside a process does not prevent an empty result from
causing another model request. Upstream reports remain open, including
[terminal polling #13733](https://github.com/openai/codex/issues/13733) and
[Desktop waiting #35259](https://github.com/openai/codex/issues/35259).
An October 2 [public experiment](https://github.com/renesugar/codex-usage-drain-lab/blob/ff70a5ad86c1538b61dbc1414ca93ffe731aa788/reports/20261002-waiting-inference-analysis.md)
reports fewer model sampling rounds when terminal re-arming stays inside
deterministic JavaScript. That supports investigating runtime waiting; it
does not establish the exact savings this patch would deliver.

The old patch helps by overriding short empty polls with a longer wait and
returning on output. A stronger design should distinguish meaningful output
from routine progress chatter; otherwise a noisy build can still repeatedly
wake the model. Cancellation, process exit, bounded checkpoints, user input,
and final-output draining must remain responsive and correct.

## Command-aware summaries

These remain substantially different from what either repository currently
does. Code already formats ordinary shell results as JSON and saves oversized
formatted output with a file reference. Its separate raw-log spooling is
conditional on debug mode. Upstream uses bounded output buffers and truncation
budgets. These mechanisms limit size; they do not systematically turn hundreds
of passing tests into a concise result while retaining useful diagnostics.
See [Code's output formatter](https://github.com/just-every/code/blob/41f66ca5d6193b32703333923c8158af12c5657a/code-rs/core/src/codex/streaming.rs)
and [Codex's output handling](https://github.com/openai/codex/blob/1ffa95ee8a02ec68f6ccd758998bcb7d4b32bcdb/codex-rs/core/src/tools/context.rs).

There is also an existing [upstream request for command-output filtering](https://github.com/openai/codex/issues/19001).
[RTK](https://github.com/rtk-ai/rtk) supplies command-specific filtering today,
making it a useful comparison baseline. Its advertised percentages concern
command output, not equivalent reductions in total cost.

The old summary implementation needs improvement: substring command detection
is too permissive, keyword diagnostics are incomplete, and it summarizes
**after truncation**, potentially losing information before selection. Those
are reasons to redesign it, not dismiss the idea. Artifact references should
provide a usable retrieval mechanism; the old payload supplies a hash rather
than a directly usable path while saying that full output is attached.

## Exact duplicate results

The patch checks exact captured output and source fingerprints, and retains
references only while the original call remains in active history. Those are
meaningful safeguards. Code's [repeated-tool-cycle guard](https://github.com/just-every/code/blob/41f66ca5d6193b32703333923c8158af12c5657a/code-rs/core/src/codex/repeated_tool_cycle.rs)
instead stops an identical consecutive cycle; it does not remove redundant
output from otherwise productive sessions. No equivalent substitution was
found in the inspected upstream paths.

The open questions are practical: how often useful duplicates occur, whether
repository hashing costs outweigh savings, and whether references remain
understandable after truncation, compaction, and resume. Active-history
membership alone does not prove that the earlier model-visible result contains
all information needed by a later request. Evaluate differing output budgets,
concurrent calls, changed source state, and preservation of result metadata.

## Lossless terminal codec

The codec has a narrower but plausible role. Repeated-line encoding can
preserve counts without sending every repeated line. However, the whole
transformation is not byte-lossless: it also removes styling and may normalize
line endings. Raw artifacts preserve the captured bytes separately. Its
synthetic tests show intended behavior, but do not establish benefits on real
workloads or model comprehension. Captured bytes may already exclude output
omitted by an earlier collection cap; an artifact cannot restore those bytes.

## Compact execution metadata and Code Mode coverage

Compact metadata is the weakest standalone opportunity. Code already uses
JSON for ordinary shell results, and upstream Code Mode returns typed execution
results. More importantly, the old patch's compression and duplicate
substitution bypass that Code Mode result path. Reviving it unchanged would
miss an important upstream execution route. See [Code Mode dispatch](https://github.com/openai/codex/blob/1ffa95ee8a02ec68f6ccd758998bcb7d4b32bcdb/codex-rs/core/src/tools/code_mode/mod.rs).

Compression hooks must preserve the typed results consumed by JavaScript.
Evaluate transformations at the boundary where results become model-visible,
rather than silently changing nested tool contracts. Metadata-only savings
should be measured separately from output compression; extra hashes and JSON
fields may offset savings, and the old implementation writes artifacts before
deciding whether the candidate is smaller.

## Recommended future investigation

Preserve the experiment and investigate waiting and command-aware summaries
first, then duplicate substitution and the codec. Use isolated comparisons
against the pinned stock implementations and, where applicable, RTK or a Code
Mode script that performs deterministic waiting and selection.

Measure model sampling requests, actual output tokens, wall time, hashing and
storage costs, diagnostic retention, artifact retrieval, and successful task
completion. Include silent long-running jobs, noisy progress, successful and
failing checks, repeated reads and searches, compaction/resume, cancellation,
and direct versus Code Mode calls. Do not infer task quality or subscription
savings merely from smaller strings or raw cached-input totals.

This investigation establishes continued relevance; it does not yet establish
production benefit.
