// Safe file access for the `fs` capabilities (RUNTIME.md section 12).
//
// The broker has already checked the canonical path (crate::targets) against every
// `only` pattern in the chain. What remains is to make sure the file opened is the
// file that was checked. Symbolic links break that: "/data/reports/q3.csv" could pass
// through a link to /etc. So every path is opened one directory at a time, each
// relative to the one before (openat), and no symbolic link is ever followed, in any
// directory or in the file itself. Because each step is relative to an open directory,
// swapping a directory for a link between the check and the open does not help: the
// swap is either before the step (and refused) or after it (and irrelevant).
//
// Reads accept only regular files, opened without blocking (a FIFO cannot hang the run),
// and refuse a file with more than one hard link, which could be a second name for a
// file outside the allowed folders. Writes go to a new temporary file in the same
// directory, are flushed, and replace the target with one atomic rename, so a crash
// never leaves half a file, and a hard link elsewhere keeps its own contents.
//
// Unix only. On other platforms every operation is refused.

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    fn c(s: &str) -> Result<CString, String> {
        CString::new(s).map_err(|_| "a file path cannot contain a NUL character".to_string())
    }

    fn errno() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    fn stat_at(dir: RawFd, name: &CString) -> Option<libc::stat> {
        // SAFETY: `st` is written by fstatat before it is read, and only if it succeeds.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            (libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) == 0).then_some(st)
        }
    }

    fn is_link(st: &libc::stat) -> bool {
        st.st_mode & libc::S_IFMT == libc::S_IFLNK
    }

    fn is_reg(st: &libc::stat) -> bool {
        st.st_mode & libc::S_IFMT == libc::S_IFREG
    }

    /// Explains a failed open of `name` (shown as `shown`) inside `dir`.
    fn open_error(dir: RawFd, name: &CString, shown: &str, err: std::io::Error) -> String {
        if stat_at(dir, name).is_some_and(|st| is_link(&st)) {
            return format!("'{shown}' is a symbolic link; links are never followed");
        }
        match err.raw_os_error() {
            Some(libc::ENOENT) => format!("'{shown}' does not exist"),
            Some(libc::ENOTDIR) => format!("'{shown}' is not a directory"),
            Some(libc::EACCES) | Some(libc::EPERM) => format!("'{shown}': permission denied"),
            _ => format!("'{shown}': {err}"),
        }
    }

    fn openat(dir: RawFd, name: &CString, flags: libc::c_int, mode: libc::c_uint) -> Result<OwnedFd, std::io::Error> {
        // SAFETY: `name` is a valid NUL-terminated string; on success the returned
        // descriptor is new and owned by nobody else.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
        if fd < 0 { Err(errno()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
    }

    /// Opens the directory that holds the file at canonical `path`, without following
    /// any link, and returns it with the file's name.
    fn open_parent(path: &str) -> Result<(OwnedFd, CString, String), String> {
        let rest = path.strip_prefix('/').ok_or_else(|| format!("'{path}' is not absolute"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        let (leaf, dirs) = parts.split_last().ok_or_else(|| format!("'{path}' names no file"))?;
        let mut cur = openat(libc::AT_FDCWD, &c("/")?, libc::O_RDONLY | libc::O_DIRECTORY, 0)
            .map_err(|e| format!("cannot open /: {e}"))?;
        let mut shown = String::new();
        for d in dirs {
            shown.push('/');
            shown.push_str(d);
            let name = c(d)?;
            cur = openat(cur.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)
                .map_err(|e| open_error(cur.as_raw_fd(), &name, &shown, e))?;
        }
        Ok((cur, c(leaf)?, path.to_string()))
    }

    pub fn read(path: &str, max: usize) -> Result<String, String> {
        let (dir, name, shown) = open_parent(path)?;
        let fd = openat(dir.as_raw_fd(), &name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0)
            .map_err(|e| open_error(dir.as_raw_fd(), &name, &shown, e))?;
        // SAFETY: `st` is written by fstat before it is read, and only if it succeeds.
        let st = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(fd.as_raw_fd(), &mut st) != 0 {
                return Err(format!("'{shown}': {}", errno()));
            }
            st
        };
        if !is_reg(&st) {
            return Err(format!("'{shown}' is not a regular file"));
        }
        if st.st_nlink > 1 {
            return Err(format!(
                "'{shown}' has {} hard links; a file with other names could be a file outside the allowed folders",
                st.st_nlink
            ));
        }
        if st.st_size as u64 > max as u64 {
            return Err(format!("'{shown}' is {} bytes, more than the limit of {max}", st.st_size));
        }
        // Back to blocking reads now that it is known to be a regular file.
        // SAFETY: plain fcntl calls on a descriptor we own.
        unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) != 0 {
                return Err(format!("'{shown}': {}", errno()));
            }
        }
        let mut buf = Vec::new();
        File::from(fd).take(max as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("'{shown}': {e}"))?;
        if buf.len() > max {
            return Err(format!("'{shown}' grew past the limit of {max} bytes while being read"));
        }
        String::from_utf8(buf).map_err(|_| format!("'{shown}' is not UTF-8 text"))
    }

    pub fn write(path: &str, content: &[u8]) -> Result<(), String> {
        let (dir, name, shown) = open_parent(path)?;
        let leaf = name.to_str().map_err(|_| format!("'{shown}' is not a valid name"))?.to_string();
        // An existing target must be a regular file; keep its permissions.
        let mode = match stat_at(dir.as_raw_fd(), &name) {
            Some(st) if is_link(&st) => return Err(format!("'{shown}' is a symbolic link; links are never followed")),
            Some(st) if !is_reg(&st) => return Err(format!("'{shown}' exists and is not a regular file")),
            Some(st) => Some(st.st_mode & 0o7777),
            None => None,
        };
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).map_err(|e| format!("no randomness for a temporary name: {e}"))?;
        let hex: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
        let tmp_name = format!(".{leaf}.{hex}.intent-tmp");
        if tmp_name.len() > 255 {
            return Err(format!("'{shown}': the file name is too long to write safely"));
        }
        let tmp = c(&tmp_name)?;
        let fd = openat(
            dir.as_raw_fd(),
            &tmp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            mode.map_or(0o644, |m| m as libc::c_uint),
        )
        .map_err(|e| open_error(dir.as_raw_fd(), &tmp, &shown, e))?;
        let remove_tmp = || {
            // SAFETY: unlinking a name we created, relative to a directory we hold open.
            unsafe { libc::unlinkat(dir.as_raw_fd(), tmp.as_ptr(), 0) };
        };
        let mut f = File::from(fd);
        let written = (|| -> Result<(), String> {
            if let Some(m) = mode {
                // SAFETY: fchmod on a descriptor we own; keeps the old file's permissions.
                if unsafe { libc::fchmod(f.as_raw_fd(), m as libc::mode_t) } != 0 {
                    return Err(format!("'{shown}': {}", errno()));
                }
            }
            f.write_all(content).map_err(|e| format!("'{shown}': {e}"))?;
            f.sync_all().map_err(|e| format!("'{shown}': {e}"))
        })();
        if let Err(e) = written {
            remove_tmp();
            return Err(e);
        }
        drop(f);
        // SAFETY: renaming within one directory we hold open; both names are valid.
        if unsafe { libc::renameat(dir.as_raw_fd(), tmp.as_ptr(), dir.as_raw_fd(), name.as_ptr()) } != 0 {
            let e = errno();
            remove_tmp();
            return Err(format!("'{shown}': {e}"));
        }
        // The rename itself must survive a crash.
        // SAFETY: fsync on a directory descriptor we own.
        if unsafe { libc::fsync(dir.as_raw_fd()) } != 0 {
            return Err(format!("'{shown}' was written, but its folder could not be flushed: {}", errno()));
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn read(_path: &str, _max: usize) -> Result<String, String> {
        Err("file access is only available on Linux and macOS".into())
    }
    pub fn write(_path: &str, _content: &[u8]) -> Result<(), String> {
        Err("file access is only available on Linux and macOS".into())
    }
}

/// Reads the UTF-8 text file at canonical `path`, at most `max` bytes.
pub fn read(path: &str, max: usize) -> Result<String, String> {
    imp::read(path, max)
}

/// Replaces the file at canonical `path` with `content`, atomically.
pub fn write(path: &str, content: &[u8]) -> Result<(), String> {
    imp::write(path, content)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("intentc-fsx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("allowed")).unwrap();
        fs::create_dir_all(d.join("outside")).unwrap();
        fs::write(d.join("outside/secret"), "SECRET").unwrap();
        // Real paths only: the test folder itself must not sit behind a link.
        fs::canonicalize(&d).unwrap()
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn reads_and_writes_regular_files() {
        let d = dir("basic");
        let f = d.join("allowed/a.txt");
        write(&s(&f), b"hello").unwrap();
        assert_eq!(read(&s(&f), 100).unwrap(), "hello");
        write(&s(&f), "héllo €".as_bytes()).unwrap();
        assert_eq!(read(&s(&f), 100).unwrap(), "héllo €");
        // No temporary file is left behind.
        let names: Vec<String> = fs::read_dir(d.join("allowed")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec!["a.txt"]);
        assert!(read(&s(&d.join("allowed/missing")), 100).unwrap_err().contains("does not exist"));
        assert!(write(&s(&d.join("nope/x")), b"x").unwrap_err().contains("does not exist"));
    }

    #[test]
    fn never_follows_a_link_in_a_folder() {
        let d = dir("dirlink");
        symlink(d.join("outside"), d.join("allowed/link")).unwrap();
        let through = d.join("allowed/link/secret");
        assert!(read(&s(&through), 100).unwrap_err().contains("symbolic link"));
        assert!(write(&s(&d.join("allowed/link/new")), b"x").unwrap_err().contains("symbolic link"));
        assert!(!d.join("outside/new").exists());
    }

    #[test]
    fn never_follows_a_link_as_the_file() {
        let d = dir("filelink");
        symlink(d.join("outside/secret"), d.join("allowed/f")).unwrap();
        assert!(read(&s(&d.join("allowed/f")), 100).unwrap_err().contains("symbolic link"));
        assert!(write(&s(&d.join("allowed/f")), b"overwrite").unwrap_err().contains("symbolic link"));
        assert_eq!(fs::read_to_string(d.join("outside/secret")).unwrap(), "SECRET");
        // A dangling link and a loop are refused the same way.
        symlink(d.join("allowed/nothing"), d.join("allowed/dangling")).unwrap();
        assert!(write(&s(&d.join("allowed/dangling")), b"x").unwrap_err().contains("symbolic link"));
        symlink(d.join("allowed/loop"), d.join("allowed/loop")).unwrap();
        assert!(read(&s(&d.join("allowed/loop")), 100).unwrap_err().contains("symbolic link"));
        assert!(read(&s(&d.join("allowed/loop/x")), 100).unwrap_err().contains("symbolic link"));
    }

    #[test]
    fn refuses_hard_links_on_read_and_keeps_them_intact_on_write() {
        let d = dir("hardlink");
        fs::hard_link(d.join("outside/secret"), d.join("allowed/h")).unwrap();
        assert!(read(&s(&d.join("allowed/h")), 100).unwrap_err().contains("hard links"));
        // Writing replaces the name, never the shared file.
        write(&s(&d.join("allowed/h")), b"new").unwrap();
        assert_eq!(fs::read_to_string(d.join("outside/secret")).unwrap(), "SECRET");
        assert_eq!(read(&s(&d.join("allowed/h")), 100).unwrap(), "new");
    }

    #[test]
    fn refuses_special_files_without_blocking() {
        let d = dir("fifo");
        let p = c_path(&d.join("allowed/pipe"));
        // SAFETY: creating a FIFO at a test path.
        assert_eq!(unsafe { libc::mkfifo(p.as_ptr(), 0o600) }, 0);
        assert!(read(&s(&d.join("allowed/pipe")), 100).unwrap_err().contains("not a regular file"));
        assert!(write(&s(&d.join("allowed/pipe")), b"x").unwrap_err().contains("not a regular file"));
        assert!(read(&s(&d.join("allowed")), 100).unwrap_err().contains("not a regular file"));
    }

    fn c_path(p: &Path) -> std::ffi::CString {
        std::ffi::CString::new(p.to_str().unwrap()).unwrap()
    }

    #[test]
    fn size_and_encoding_limits() {
        let d = dir("limits");
        fs::write(d.join("allowed/big"), vec![b'a'; 101]).unwrap();
        assert!(read(&s(&d.join("allowed/big")), 100).unwrap_err().contains("more than the limit"));
        assert_eq!(read(&s(&d.join("allowed/big")), 101).unwrap().len(), 101);
        fs::write(d.join("allowed/bin"), [0xff, 0xfe]).unwrap();
        assert!(read(&s(&d.join("allowed/bin")), 100).unwrap_err().contains("not UTF-8"));
    }

    #[test]
    fn keeps_permissions_of_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("perms");
        let f = d.join("allowed/p");
        fs::write(&f, "x").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        write(&s(&f), b"y").unwrap();
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// A folder that keeps changing between a real folder and a link to the outside
    /// folder, while reads run: reads may succeed (real folder) or be refused (link),
    /// but must never return the outside file's contents.
    #[test]
    fn a_folder_swapped_for_a_link_mid_read_never_leaks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let d = dir("race");
        let store = d.join("allowed/.store");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("secret"), "ALLOWED").unwrap();
        fs::write(d.join("outside/secret"), "SECRET").unwrap();
        let sw = d.join("allowed/sw");
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let (stop, d, sw, store) = (stop.clone(), d.clone(), sw.clone(), store.clone());
            std::thread::spawn(move || {
                let mut rounds = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    fs::rename(&store, &sw).unwrap(); // a real folder
                    fs::rename(&sw, &store).unwrap();
                    symlink(d.join("outside"), &sw).unwrap(); // a link to the outside
                    fs::remove_file(&sw).unwrap();
                    rounds += 1;
                }
                rounds
            })
        };
        let target = s(&sw.join("secret"));
        let (mut allowed, mut refused) = (0, 0);
        for _ in 0..20000 {
            match read(&target, 100) {
                Ok(text) => {
                    assert_eq!(text, "ALLOWED", "a read through a swapped folder leaked");
                    allowed += 1;
                }
                Err(_) => refused += 1,
            }
        }
        stop.store(true, Ordering::SeqCst);
        let rounds = swapper.join().unwrap();
        assert!(rounds > 100, "the swapper only ran {rounds} rounds");
        assert!(refused > 0, "no read met the link ({allowed} allowed)");
        println!("{rounds} swaps; {allowed} reads of the real folder, {refused} refused");
    }
}
