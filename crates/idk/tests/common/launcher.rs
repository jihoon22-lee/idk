//! Select the exact runtime candidate for CLI/host fixtures. Fixture/library
//! setup remains in the test harness; child processes use this one executable.
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
