//! Select the exact runtime candidate for CLI/host fixtures. Fixture/library
//! setup remains in the test harness; child processes use this one executable.
#![allow(dead_code)] // Each binary that embeds this module uses its own subset.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub fn path() -> &'static Path {
    static LAUNCHER: OnceLock<PathBuf> = OnceLock::new();
    LAUNCHER.get_or_init(|| {
        let path = std::env::var_os("IDK_TEST_LAUNCHER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_idk")));
        assert!(path.is_absolute(), "IDK_TEST_LAUNCHER must be absolute");
        assert!(
            path.is_file(),
            "runtime launcher must be an existing file: {}",
            path.display()
        );
        path
    })
}
/// The real csh-family binary integration tests require. Resolves
/// `IDK_TEST_SHELL`/`IDK_TEST_TCSH` then the usual install paths; panics with
/// an explicit reason when no usable binary exists instead of failing deep
/// inside trust approval or PTY spawn.
pub fn test_shell() -> PathBuf {
    static SHELL: OnceLock<PathBuf> = OnceLock::new();
    SHELL
        .get_or_init(|| {
            [
                std::env::var_os("IDK_TEST_SHELL")
                    .or_else(|| std::env::var_os("IDK_TEST_TCSH"))
                    .map(PathBuf::from),
                Some(PathBuf::from("/usr/bin/tcsh")),
                Some(PathBuf::from("/bin/tcsh")),
            ]
            .into_iter()
            .flatten()
            .find(|path| path.is_file())
            .expect("a real csh/tcsh is required for this test; install tcsh or set IDK_TEST_SHELL")
        })
        .clone()
}
