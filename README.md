# drive

The one way to check, test, document and bench a Cargo workspace, for an agent, a person and CI
alike. Each command runs a fixed set of cargo commands over the whole workspace, so an edit
rebuilds each crate once, and a step that already passed on the same files does not run again.

## Install

```
cargo install --locked --git https://github.com/xorza/drive
```

This puts `cargo-drive` on `PATH`, which cargo runs as `cargo drive`. Run the same command again
to update it. A workspace names the driver it needs in its drive table, and an older or newer one
refuses to run:

```toml
[workspace.metadata.drive]   # or [package.metadata.drive] in a single-package root
version = "0.2.3"
```

## Use

Run it from the workspace root:

```
cargo drive check          # after each change: fmt, then clippy on every target with every feature
cargo drive test [args]    # the tests; nextest's arguments select some
cargo drive bench [args]   # the benches; criterion's arguments select some
cargo drive gate           # every check CI runs; CI adds --ci
cargo drive run NAME       # a command of the workspace's own drive table
cargo drive package        # the release build of the binaries the table names
cargo drive vocabulary     # every declared name and word, for a review: target/drive/vocabulary.txt
cargo drive surface        # the root package's public API, for a review: .notes/API_SURFACE.md
```

A run ends on one line: `ok`, or `FAIL` with the failed steps and the first error, and the file
`target/drive/last-<command>.txt` that holds all of the run. Above it, each failed step shows its
errors, or each failed test with where and why it panicked. On a terminal each step prints as it
goes; on a pipe, as an agent reads it, only each step's line and that digest, and the output of a
bench or a program, which is its result. So a bare `cargo drive check` prints a few lines and
needs no `| tail`.

A command of the workspace's own table can name the texts that mark an error in its output,
`errors = ["ERROR", "panicked at"]`: a failed step of it shows those lines, or its last lines where
none holds one.
