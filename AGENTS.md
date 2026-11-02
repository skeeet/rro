# Project instructions for agents

User-facing replies: short, direct English. No filler. This style does not apply to code or documents.

This parent chat coordinates, specifies, reviews, and integrates. Use bounded subagents for implementation features. Shared checkout: each worker owns only its assigned files; preserve other workers' changes. Do not create other chats or send external messages.

Read `docs/SPEC.md` before implementation. `docs/AGENT_GUIDE.md` provides agent workflows; `docs/release-contract.json` defines release-suite identities and limits. Human documentation starts at `README.md` and `docs/REBUILD.md`. Both audiences share the specification and evidence; do not duplicate their authority.

Runtime must be one macOS executable, with no Python, virtual environment, Homebrew library, or downloaded runtime. Keep core, CLI, and codec architecture portable to Windows and Linux. Project-authored Python is allowed only for development verification, benchmarking, and release tooling. External research implementations and assets do not belong in this repository or release archives. Preserve citations, attribution, license terms, and source hashes in `docs/REFERENCE.md`, `docs/reference-manifest.json`, and `THIRD_PARTY_NOTICES`. Use anonymous report metadata and path-remapped release builds; local usernames, hostnames, private paths, and personal Git identities must not enter source or release artifacts.

Keep generated outputs within this workspace. After final packaging and verification, remove disposable build cache and verification scratch while retaining final release artifacts. Do not use `/tmp`. Use `reports/work/` for scratch data; leave `target/`, `dist/`, editor settings, and installed agent skills untracked. Preserve user-installed local skills and lint configuration. No paid services or generation are required.

Verify feature correctness, invalid-input handling, numerical agreement, and release linking before declaring completion. Record actual results separately from acceptance targets. Do not silently relax numeric tolerances or substitute an ordinary cubic resizer for the specified cubic-derived camera. Use configured Rust lints and the available Rust review/best-practices skills; install identities may be recorded locally in untracked `skills-lock.json`.
