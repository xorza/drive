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
version = "0.2.2"
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
```

A run ends on one line: `ok`, or `FAIL` with the failed steps and the first error, and the file
`target/drive/last-<command>.txt` that holds all the run printed. So `cargo drive check | tail -1`
is enough, and on a failure that file holds the rest.
