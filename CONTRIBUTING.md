# Contributing to EVE Preview Manager

Thanks for wanting to help improve EVE Preview Manager.

## Before you start

Bug reports and feature ideas are always welcome as issues. For a larger feature or rewrite, opening an issue first is helpful: it gives us a chance to get on the same page before you spend a lot of time on it.

## Pull requests

1. Fork the repository and create a branch from `dev`.
2. Make your changes.
3. Run the relevant checks locally:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo test --locked --all-features
   ```

4. Open a pull request against the `dev` branch.
5. Give a quick rundown of what changed, why, and how you tested it. Screenshots or a short recording are especially helpful for visible UI changes.

Please mention anything unfinished or any checks you could not run. Update tests and documentation when needed.

## Development setup

The project needs Rust/Cargo plus the system libraries listed in the [README](README.md#build-from-source).

## Review

Reviews may take a little time because this is a one-person project. I may ask questions or suggest changes before merging.
