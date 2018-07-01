# Rust submodule update tool.

Work in progress, though mostly functional.

A tool to help update git submodules in the rust repo and test the updates.

## Usage

When run in a terminal, it will use interactive prompts to verify certain
steps.  It will do everything up to just before committing and pushing a PR to
GitHub.

Basic example, run inside the rust repo:

`subup src/tools/cargo src/tools/rls`

Example of updating a submodule on the beta branch:

`subup --rust-branch beta rust-1.28.0:src/tools/cargo`
