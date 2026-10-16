# intentc — the Intent compiler (v0.1, front end only)

This is phase 1 of the Intent build plan: a lexer, parser and type checker,
written in Rust with no external dependencies. It does not run programs yet.
It checks them and rejects the ones that break Intent's rules.

## What it enforces today

| Code | Meaning |
| ---- | ------- |
| E001 | A model's `Proposed<T>` reaches an action or a non-Proposed return without `validate` |
| E002 | An action is performed without a matching capability in scope |
| E003 | A capability is widened: passed, assigned or returned with more authority than it holds |
| E100 | Undefined name or function |
| E101 | Type mismatch or wrong number of arguments |
| E102 | Unknown type |
| E900 | Lexer error |
| E901 | Syntax error |

E004 to E008 from the spec (budgets, unbounded loops, untrusted instructions,
blind_to leaks, evidence) are not implemented yet.

## Capability limits

    fn small_refund(pay: cap payments.refund(max: 50 USD), r: Refund) { ... }

A capability may be narrowed (500 USD -> 50 USD, unlimited -> 50 USD) and never
widened (50 USD -> 500 USD, 50 USD -> unlimited). Limits in different units are
never converted. Only `max` is supported, and limits are whole numbers.

The checker controls how much authority code is handed. It does not yet stop a
single action from exceeding the limit at runtime (e.g. refunding 600 through a
500 USD capability); that check belongs to the runtime capability broker.

## Requirements

Rust, installed from https://rustup.rs. Developed with cargo 1.97.0.

## Use

    cargo build
    ./target/debug/intentc check tests/fixtures/pass/refund.int
    cargo test

`intentc check` exits 0 when the program is valid and 1 when it has errors.

## Tests

`tests/fixtures/pass/` holds programs that must compile.
`tests/fixtures/fail/` holds programs that must be rejected. Each starts with
`// expect: CODE`, and the test fails unless the compiler reports exactly that code.
To add a rule, write the failing program first, then make the checker reject it.

## Layout

    src/lexer.rs    source text -> tokens
    src/parser.rs   tokens -> AST (grammar is in the file header)
    src/ast.rs      AST and types
    src/checker.rs  type and safety rules
    src/main.rs     command-line interface
