# Homebrew tap and releases

KBorg ships through a Homebrew tap that builds from the tagged source, so no per-platform binaries or notarization are needed for brew installs. The formula lives in this repository at [`packaging/homebrew/kborg.rb`](../packaging/homebrew/kborg.rb) and is copied to the tap repository on each release.

## One-time setup

1. Make `c0dewars/kborg` public (the formula downloads GitHub's source tarball without credentials).
2. Create a public repository named `c0dewars/homebrew-tap` with a `Formula/` directory. Homebrew maps `brew install c0dewars/tap/kborg` to `github.com/c0dewars/homebrew-tap/Formula/kborg.rb`.

## Each release

```sh
# 1. Bump `version` in Cargo.toml, update docs/RELEASE_NOTES.md, merge to main.
git tag -a v0.4.0 -m "KBorg 0.4.0" && git push origin v0.4.0

# 2. Pin the formula to the tag's tarball (computes the SHA-256).
python3 scripts/update-homebrew-formula.py 0.4.0

# 3. Publish the formula to the tap.
cp packaging/homebrew/kborg.rb ../homebrew-tap/Formula/kborg.rb
(cd ../homebrew-tap && git add Formula/kborg.rb && git commit -m "kborg 0.4.0" && git push)

# 4. Verify like a user would.
brew install c0dewars/tap/kborg && brew test kborg && kborg --version
```

The formula builds with Homebrew's Rust (KBorg's minimum is 1.89), runs `kborg --version` and an offline `kborg --demo list pods` as its test, and installs bash, zsh and fish completions. A local build takes about a minute on Apple silicon.
