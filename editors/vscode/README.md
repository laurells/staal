# Staal for VS Code

Highlighting for `.staal` files, and from `staalc lsp`: errors as you type,
completion and formatting. The errors are the ones `staalc check` prints, from the
same code, so the editor and the compiler never disagree.

The extension is not on the Marketplace. To install it from this folder:

    npm ci
    npx @vscode/vsce package --allow-missing-repository --skip-license -o staal.vsix
    code --install-extension staal.vsix

It runs `staalc lsp`, finding `staalc` on your PATH; set `staal.server.path` to use
another executable. If it cannot start the server it says so.

`npm test` runs the grammar with VS Code's own tokenizer over a sample and over every
Staal file in the repository, drives `staalc lsp` with VS Code's protocol library
(build `staalc` first, or set `STAALC` to it), and checks how the extension starts,
restarts and stops its server. `cargo test --test editor_test` keeps the grammar's
reserved words and built-ins identical to the compiler's.

Not yet tested here: running inside VS Code itself, which needs a VS Code download.
