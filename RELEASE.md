# Releasing

How costsee is published. The version follows the existing 0.1.x patch
pattern: each feature is released as the next patch unless there is a reason
to bump minor.

## 1. Check the working tree

All intended changes must be committed, the checks must pass, and the branch
must be clean before tagging:

```console
cargo fmt --check
cargo test
cargo clippy --all-targets
git status
```

## 2. Bump the version

Choose the next `0.1.x`, set it in `Cargo.toml`, then rebuild so `Cargo.lock`
records it:

```console
# edit Cargo.toml: version = "0.1.N"
cargo build
```

Confirm both files changed:

```console
git diff Cargo.toml Cargo.lock
```

## 3. Commit and tag

```console
git add Cargo.toml Cargo.lock
git commit -m "release 0.1.N"
git tag v0.1.N
```

## 4. Push

```console
git push origin main --tags
```

## 5. Publish to crates.io

Dry-run first to catch packaging problems without publishing:

```console
cargo publish --dry-run
cargo publish
```

## 6. Create the GitHub release

Follow the previous release's shape, comparing against the previous tag:

```console
gh release create v0.1.N \
  --title v0.1.N \
  --notes "**Full Changelog**: https://github.com/lef237/costsee/compare/v0.1.M...v0.1.N"
```

where `v0.1.M` is the previously released tag.
