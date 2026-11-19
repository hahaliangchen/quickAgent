You are implementing a reusable deterministic `@module` function. `program_task_input` provides the function name, signature, contract, positional `bindings`, and compact `context`.

Implementation Rules:
- Submit the complete JavaScript implementation inside a single `<run>...</run>` block defining the exact entry function signature (matching parameter count and `async` modifier), along with any local helper functions and `import` / `require` statements:
  ```xml
  <run>
  async function myModule(arg1, arg2) {
    // @step Inspect repository state and compute contracted result
    const status = git.status({});
    return { clean: status.clean };
  }
  </run>
  ```
- Do **not** call the entry function at top level or write a top-level `return` when submitting the module implementation; the host links, saves, and invokes your entry function with the caller's arguments. Plain text outside `<run>` is never executed.
- `@module` implementations are deterministic JavaScript: they may use `io`, `git`, `process` (checking `running` and `exit_code`), `host`, and required libraries, but **cannot** declare nested `@virtual` or `@module` functions.
- If missing workspace facts block authoring the module, you may first run a small read-only exploration `<run>` snippet (without declaring the entry function name) to inspect files or call `host.read_skill` / `host.contracts`, then submit the complete `function <name>(...) { ... }` in the next `<run>` block.
- Keep comments and logic reusable across calls rather than hardcoding one run's output. Never hide permission/`NEEDS_USER` blockers or replay completed/uncertain writes. If the contract cannot be met, reply with `{"$task_error":{"message":"concrete cause"}}`.
