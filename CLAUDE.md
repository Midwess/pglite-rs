# CLAUDE.md

## Theory

### Coding convention
- Maximize Cohesion: Group related data and behavior tightly together. Do not scatter related logic across different files or scopes.
- **Least New Definitions (precedes Struct-First)**: Minimize new structs, traits, modules, and type aliases. Before introducing ANY new technical definition, scan every relevant existing module for a host that already owns adjacent data or behavior; if a fit exists, attach the new logic there as a method or associated function on the existing struct. New definitions are a maintenance cost — pay it only when no existing home is a natural fit AND the logic represents a genuinely new domain concept. Reuse > create. This rule TAKES PRECEDENCE over Struct-First when the two collide: if attaching a method to an existing struct avoids spawning a new struct/module, attach to the existing struct. Trigger a codebase scan (`grep`, module tree walk) BEFORE the first line of new code is written; record (mentally or in PR description) which existing struct was considered and why it was rejected if you ultimately do create a new one. For ephemeral grouping of return values inside a single function, prefer tuples or destructured `let` over a one-off struct. Reserve struct creation for values that genuinely cross module boundaries or carry behavior.
- Struct-First Design: By default, all functions MUST be associated with a specific struct (e.g., as methods or associated functions). Avoid free-floating functions wherever possible. Apply AFTER the Least New Definitions scan has confirmed a new struct is warranted; if an existing struct can host the function, prefer that over a new struct.
- Strict Placement: Every function must be placed exactly within its relevant domain module and attached to the correct struct.
- Isolate New Domains: If — and only if — the Least New Definitions scan has confirmed no existing struct or module is a natural fit, define a brand new, narrowly scoped module for the new logic. DO NOT force unrelated logic into an existing module, but also DO NOT spawn a new module when an existing one would have hosted the logic cleanly.
- Extract Agnostic Logic: If a function is purely utility-based and genuinely does not relate to any specific domain struct or module, it must be defined completely outside of the domain logic (e.g., in an isolated utils or common module).
- Strict Lock Encapsulation: ALL synchronization primitives (e.g., Mutex, RwLock) MUST be completely hidden inside internal structs. NEVER expose a lock to the public API.
- Enforce Interior Mutability: The public interface of the struct must ONLY expose immutable methods (&self). The internal implementation will handle acquiring the lock, dropping it immediately, and mutating the inner state.
- Zero External Deadlocks: The outside caller must never have to manage locking logic. Lock scopes must be kept extremely brief and self-contained within the method execution to guarantee deadlock-free operations.
- No Fake Inner Structs: Do NOT create an `XxxInner` struct just to hold fields so the outer struct can be `Clone`. That is an anti-pattern — it hides which fields actually need shared mutable access. Instead, each field that needs mutation wraps itself in `Arc<Mutex<T>>` or `Arc<RwLock<T>>` directly on the struct. Fields that are already `Arc`-based are already `Clone`. Non-Clone fields (e.g., `JoinHandle<()>`) are wrapped in `Arc<JoinHandle<()>>` individually. The struct itself then gets `#[derive(Clone)]` with no indirection. Example: `replay_buffer: Arc<Mutex<VecDeque<Bytes>>>` not `inner: Arc<AgentInner>`.
- Never do inline code comments, if saw any unnecessary comments, remove them
- **Cancellation MUST use `core_services::utils::cancellation::CancellationToken`.** It is the single project-wide cancellation primitive. Re-export at `crate::agent::CancellationToken` for convenience. Pattern for any struct that spawns a background task: hold `cancellation: CancellationToken` field; in the spawned task's `tokio::select!`, the first biased arm is `() = self.cancellation.cancelled() => return;`; `shutdown(&self)` calls `self.cancellation.cancel()`. Drop-guard via `cancellation.drop_guard()` returns a `DropGuard` whose `Drop` impl calls `.cancel()` — use this when an outer struct owns a child whose tasks should die on outer-drop. Child tokens via `parent.child_token()` propagate cancel down the tree (parent.cancel() → child.cancel() recursively). Never re-introduce a separate `AbortOnDrop`-style `JoinHandle` wrapper — `CancellationToken` covers the same RAII semantics without leaking tokio details. Cross-task waiting on cancel: `future.with_cancel(&token).await` returns `Err(TaskErrors::Cancelled)` if the token fires.

## Coding phase

### Error handling
- When code rust, always use question mark.
- Unified all error by using thiserror crates as best practice.

### Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.
