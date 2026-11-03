# Reliability fixes

Goal: enforce the existing grading, tool, proof, and budget contracts using the smallest changes that reproduce and fix the reviewed failures. Preserve existing crate interfaces where possible; do not change rollback policy or introduce crash resume without separate evidence.

1. `rof`: make verifier artifacts fresh per attempt and reject verifier infrastructure failure even when an old reward exists. Test reused output directories and verifier failure.
2. `agent-loop`: reject truncated proof patches before restoration; enforce the action limit before each dispatch; retain action charges across rollback; charge cumulative fractional-cent spend. Test large patches, multi-call batches and repeated small charges.
3. `tools-std`: enforce secret denial for recursive search and resolved aliases; preserve quoted command arguments and bounded raw exec output; keep timeout/cancellation active through pipe draining. Test each reproduced failure.
4. `eval` and `rof`: record nonsecret configuration and per-attempt task and image identities where available; test differing configurations. Preserve old report deserialization.
5. Enforce the deadline for proof commands, restoring the pre-proof patch on timeout or startup failure. Review cross-crate behavior, update docs, and run scoped tests, Clippy and formatting.

Deferred: change default rollback semantics, implement WAL resume, alter steering, and build new orchestration. Validate those separately before changing behavior.
