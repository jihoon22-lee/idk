# Native distribution notices

`collect-native-notices.py collect` reads the locked Cargo target graph and the
reviewed runtime policy without network access. It collects complete published
LICENSE/LICENCE, COPYING, NOTICE, COPYRIGHT, AUTHORS and CREDITS files, legal-text
directories, third-party notice files, and any declared `license_file`. A missing
license, empty/non-UTF-8 text, escaping path, or source symlink fails collection.
Workspace members declaring MIT may use the repository's MIT `LICENSE`.

The Cargo inventory includes build and test dependencies. It is deliberately
broader than a link map; listing a crate does not claim that its object code is
present in the executable. Every collected document has a source digest and a
byte offset/length/digest in the combined notice text. All source paths in the
inventory are relative to their component, not the builder's home directory.

`distribution_notices_complete` is emitted only after the full document
collection and the pinned runtime checks succeed. It records this collection
scope and its reviewed runtime coverage; it does not substitute for release
approval or establish publisher trust.

## Rust 1.97.1 static runtime coverage

The policy fixes Rust commit `8bab26f4f68e0e26f0bb7960be334d5b520ea452` and target
`x86_64-unknown-linux-musl`. All 38 installed target `.rlib` and self-contained
runtime artifacts are checked by filename, size and SHA-256 before notices can
be marked complete. A different toolchain or runtime component requires a new
policy review, even if a version string was left unchanged.

- Rust's installed `COPYRIGHT-library.html` supplies its in-tree and dependency
  attribution inventory. All visible HTML text, including preformatted license
  text, is preserved; no linked resources or scripts are loaded. The original
  HTML digest and conversion format are recorded. The Apache, MIT, BSD, Unicode
  and LLVM exception reference texts also accompany it.
- The Rust-vendored compiler-builtins `0.1.160` and libm use the exact source in
  that Rust commit. A registry crate with the same version is not assumed to be
  the same vendored source. Its full license and source attribution comments
  are included. See the [vendored manifest](https://github.com/rust-lang/rust/blob/8bab26f4f68e0e26f0bb7960be334d5b520ea452/library/compiler-builtins/compiler-builtins/Cargo.toml).
- musl `1.2.5` includes Rust's documented build-script patches. Its full
  `COPYRIGHT` and additional source-file notices cover the separately attributed
  regex, math, crypt and sorting implementations, including permissive license
  texts located in those source files. See the [Rust musl build script](https://github.com/rust-lang/rust/blob/8bab26f4f68e0e26f0bb7960be334d5b520ea452/src/ci/docker/scripts/musl.sh).
- LLVM libunwind, compiler-rt and its CRT startup objects come from Rust's LLVM
  revision `dcc3606807c989700e0ac1cac18c31741bcd40d9`. Their full licenses, credits
  and additional runtime source comments are included. Rust's
  [CRT build step](https://github.com/rust-lang/rust/blob/8bab26f4f68e0e26f0bb7960be334d5b520ea452/src/bootstrap/src/core/build_steps/llvm.rs#L1609-L1665)
  identifies compiler-rt as the CRT source. The GCC name in an object's compiler
  metadata is not evidence that GCC runtime code was linked.

`source-evidence.json` records the inspected upstream source identities and the
comment-block selection used for the runtime supplements. The supplement keeps
whole matching C block or contiguous `//` comment blocks, with component-relative
paths and source hashes. These are published upstream sources, not user project
sources. The vendored texts and policy are build inputs; no updater downloads or
refreshes them during a normal build.

When updating the toolchain, review its upstream bootstrap/runtime dependency
sources, regenerate the source evidence and notices from those exact revisions,
and replace artifact/document pins together. Compare the resulting license and
notice changes before enabling completeness for the new toolchain. A Cargo graph
change is collected automatically; a missing newly introduced license fails.

## Candidate and archive identity

The native input fingerprint includes Cargo manifests/lockfile, the pinned
Rust toolchain declaration, `LICENSE`, `crates/**`, all three native packaging
scripts, this policy directory, and repository Cargo configuration. Source and
input fingerprints are checked before and after building. These checks detect
changed inputs; they are not an atomic filesystem snapshot or a hermetic build
claim.

`IDK_EXPECT_MAIN_SHA` requires an entirely clean checkout and exact equality of
HEAD, the supplied full SHA, and local `refs/remotes/origin/main` before and after
the build. Its method is recorded as `expected-sha-and-local-origin-main`; the
build does not fetch or claim that the local tracking ref is a fresh network
observation. The caller supplies and verifies the intended main SHA. Without
that explicit gate, `main_sha_verified` remains false.

`build-native-bundle.sh [candidate-dir] [output.tar.gz]` validates the existing
candidate and preserves its binary and manifest bytes. The archive contains
exactly five regular USTAR files, in lexical order: the executable, its manifest,
the four-file checksum list, the license inventory, and the combined notices.
The executable mode is 0755; other modes are 0644. Owner IDs and mtimes are zero,
owner names are empty, and gzip has zero mtime and no filename. No install or
postinstall script is present. Limits match the Rust reader: 64 MiB compressed,
128 MiB expanded including TAR framing, 64 MiB binary/notices, 16 MiB inventory,
and 1 MiB manifest/checksum list.

The outer `.sha256` file is an integrity aid. An expected bundle digest still
needs a separately trusted delivery channel. Bundle creation does not publish,
install, update, stop a host, modify user configuration, or execute the binary.
