# Captured language diagnostics

Captured on Linux on 2026-11-15 from intentionally failing, dependency-free examples.
These are real tool outputs, not evidence that a user project was checked.
The fixture root in the Clippy progress line is replaced with `$FIXTURE`.

Versions:

- rustc: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- tsc: `Version 5.9.3`
- ruff: `ruff 0.16.3`
- go: `go version go1.27.4 linux/amd64`
- clippy: `clippy 0.1.99 (58cf8f9c35 2026-10-13)`

Reproduction (run in separate empty project directories):

- Rust `main.rs`: `fn main() { let _: u32 = "wrong"; }`; run `rustc --color never main.rs` and `rustc --color never --error-format short main.rs`.
- Clippy Cargo binary `src/main.rs`: `fn main() { println!("{}", 1 == 1); }`; run `cargo clippy --offline` with color disabled.
- TypeScript `main.ts`: `const value: number = "wrong";`; run `tsc --pretty false --noEmit main.ts`.
- Python `main.py`: `import os`; run `ruff check --isolated --output-format concise main.py`.
- Go module `example.test/diagnostic`, `main.go`: the three lines `package main`, `import "fmt"`, `func main() { fmt.Printf("%d", "wrong") }`; run `go vet ./...`.

The tests check native rule text, positions, unknown severity as note, raw log/run identity,
and identical results for whole output versus one-byte chunks. No external tool is required to replay them.
