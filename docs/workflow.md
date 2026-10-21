# Development workflow

How AI agents and the human developer collaborate on this repository.
Complements the root `AGENTS.md` rules (especially: agents only commit when
explicitly asked by the human operator).

This is a research project: no production users, no SLA, reversible
deploys (D-006). The process is sized for that: the default path from idea
to commit is **one agent, one pass, one human scan**.

## The loop

1. **Build.** One agent implements the task (scope from
   [plan.md](plan.md)), runs the repo's checks (Rust workspace: expected to
   be `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, plus a
   wasm32 build of the Worker; the README lists the exact commands once the
   toolchain lands in M1), and ends with a short note: what changed, what was
   verified. Any change to encoder output is verified by decoding with an
   independent reference decoder, not only our own.
2. **Commit.** The human scans the note and the diff at whatever depth the
   change warrants, and either commits directly or asks the agent to commit.

That is the whole gate. There are no mandatory review passes, no multi-agent
review structure, and no verification-of-the-verification.

## Ground rules

- **Agents only commit when explicitly asked** by the human operator. Otherwise,
  the working tree is the handoff.
- **Don't hand off broken.** Checks pass before you end your turn; if they
  don't, say so plainly instead of papering over it.
- **One stream of work at a time.** Check `git status` first; if there are
  changes you didn't make, you're iterating on in-flight work, not starting
  fresh.
- **Scratch files stay out of the tree.**
- **Shared measurement hosts.** If the machine uses `hostlock` (see the user's
  instructions), run builds and test suites under `hostlock shared`. CPU
  benchmarks are measurements, so run them under `hostlock exclusive` so other
  heavy work cannot skew them. Size-only measurements are deterministic and
  need only a shared hold.
- **Fix the docs the change makes wrong** (status paragraph, plan checkbox,
  affected doc) in the same change. Nothing more is owed.

## Reviews happen on demand, not by default

The human asks for a review when a change warrants one. When asked:

- One agent, one pass, over the whole uncommitted diff.
- Hunt real defects — data loss or corruption, security, broken behavior —
  not style, ceremony, or missing log entries.
- Findings are file:line claims ranked by severity. A clean review is a valid
  result.
- Fix what you find directly unless the human asked for a report only.

## When to go heavy

Some changes carry real blast radius: anything that can destroy user data,
corrupt a published artifact, or open a security hole. For those the human may
explicitly ask for the heavyweight treatment — multi-agent review, an
adversarial challenge pass, fix/verify rounds. That escalation is the human's
call to make; agents don't self-escalate beyond one pass.
