# cargo-groups

A tool for running cargo commands on groups of crates in a workspace.

## Get Started

Install with:

```
cargo install cargo-groups
```

Then add groups to your `Cargo.toml`:

```toml
[workspace.metadata.groups]
tools = ["path:crates/foo-debugger", "path:crates/foo-compiler"]
binaries = ["path:crates/foo", "path:crates/bar"]
```

Then run the cargo command:

```agsl
cargo groups build tools
```

You can use globs in your group definitions:

```toml
[workspace.metadata.groups]
foo = ["path:crates/foo-*"]
```

You can add crates via their crate name with the `pkg:` prefix and via their path
with the `path:` prefix:

```toml
[workspace.metadata.groups]
foo = ["pkg:foo*", "path:crates/foo-*"]
```
