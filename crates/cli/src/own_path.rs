//! Where this program really lives.
//!
//! Everything chrono reads at run time lies beside its own executable: `chrono_hook.dll`, the calendars,
//! the presets and the licence files. `std::env::current_exe` answers with the path the program was
//! STARTED by, and when that is a symbolic link it is the link, in another folder. chrono started through
//! a link looked for all of them there and found none: every session stopped with `core.hook_dll_missing`
//! and `chrono calendars` with `calendars.folder_missing` (measured 2026-11-02 on the 0.5.0 package, the
//! way WinGet installs a portable package when its manifest lets it make a link).
//!
//! Only the executable itself is followed. A link somewhere up the folder path needs nothing, because a
//! file beside the program is beside it through that folder as well. A hard link has no other name to
//! follow, so a program reached through one finds its files only when they were linked beside it too.

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};

/// The longest path the Win32 calls that take one without the `\\?\` prefix accept (`MAX_PATH`, the
/// terminating zero included). A longer real path keeps the prefix, because the hook's path travels to
/// `LoadLibraryW` in the target, and the target does not opt into long paths.
const MAX_PATH: usize = 260;

/// This executable, followed through a symbolic link to the file it points to.
pub(crate) fn own_exe() -> std::io::Result<PathBuf> {
    Ok(followed(&std::env::current_exe()?))
}

/// `path` itself, or the file it points to when it is a symbolic link. A link that cannot be followed
/// stays as it is, so whatever goes wrong next names the path the program was started by.
pub(crate) fn followed(path: &Path) -> PathBuf {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if !is_link {
        return path.to_path_buf();
    }
    std::fs::canonicalize(path).map_or_else(|_| path.to_path_buf(), |real| plain(&real))
}

/// What `canonicalize` answers, `\\?\C:\x` or `\\?\UNC\server\share\x`, as `C:\x` or `\\server\share\x`
/// when it fits in `MAX_PATH`: the form every other path in the program has, and the one a message shows.
fn plain(real: &Path) -> PathBuf {
    let mut parts = real.components();
    let head = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => format!("{}:\\", char::from(letter)),
            Prefix::VerbatimUNC(server, share) => {
                format!(r"\\{}\{}\", server.to_string_lossy(), share.to_string_lossy())
            }
            _ => return real.to_path_buf(),
        },
        _ => return real.to_path_buf(),
    };
    let mut out = PathBuf::from(head);
    for part in parts.filter(|part| !matches!(part, Component::RootDir)) {
        out.push(part);
    }
    if out.as_os_str().encode_wide().count() >= MAX_PATH { real.to_path_buf() } else { out }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH, followed, plain};
    use std::path::{Path, PathBuf};

    #[test]
    fn a_verbatim_disk_path_loses_its_prefix() {
        assert_eq!(plain(Path::new(r"\\?\C:\Tools\chrono-cli\chrono.exe")), PathBuf::from(r"C:\Tools\chrono-cli\chrono.exe"));
    }

    #[test]
    fn a_verbatim_share_path_becomes_a_share_path() {
        assert_eq!(plain(Path::new(r"\\?\UNC\server\share\cli\chrono.exe")), PathBuf::from(r"\\server\share\cli\chrono.exe"));
    }

    #[test]
    fn a_path_without_the_prefix_is_left_alone() {
        assert_eq!(plain(Path::new(r"C:\Tools\chrono.exe")), PathBuf::from(r"C:\Tools\chrono.exe"));
    }

    #[test]
    fn a_real_path_too_long_without_the_prefix_keeps_it() {
        let long = format!(r"\\?\C:\{}\chrono.exe", "d".repeat(MAX_PATH));
        assert_eq!(plain(Path::new(&long)), PathBuf::from(&long));
    }

    #[test]
    fn a_file_that_is_no_link_is_its_own_answer() {
        let me = std::env::current_exe().expect("the test binary knows its own path");
        assert_eq!(followed(&me), me);
        let gone = Path::new(r"C:\no-such-folder-for-chrono\chrono.exe");
        assert_eq!(followed(gone), gone);
    }
}
