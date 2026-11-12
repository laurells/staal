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
// The same traversal is offered from a directory the caller has already opened
// (`Root`): every path beneath it is opened one directory at a time from that handle,
// never resolved by path, and may not name `..`, `.` or an empty part, so nothing it
// opens can lie outside that directory, wherever that directory is moved meanwhile.
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
        if fd < 0 {
        Err(errno())
    } else {
        // SAFETY: `fd` is the new descriptor openat just returned, owned by nobody else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
    }

    /// Opens each of `dirs` in turn, starting from the directory `start`, without
    /// following any link; `base` is how `start` is shown in messages ("" for /). With
    /// no `dirs`, a new handle on `start` itself.
    fn open_dirs(start: RawFd, dirs: &[&str], base: &str) -> Result<OwnedFd, String> {
        let shown_base = if base.is_empty() { "/" } else { base };
        let mut cur = openat(start, &c(".")?, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(|e| format!("cannot open '{shown_base}': {e}"))?;
        let mut shown = base.to_string();
        for d in dirs {
            shown.push('/');
            shown.push_str(d);
            let name = c(d)?;
            cur = openat(cur.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)
                .map_err(|e| open_error(cur.as_raw_fd(), &name, &shown, e))?;
        }
        Ok(cur)
    }

    /// Opens the directory that holds the file at canonical `path`, without following
    /// any link, and returns it with the file's name.
    fn open_parent(path: &str) -> Result<(OwnedFd, CString, String), String> {
        let rest = path.strip_prefix('/').ok_or_else(|| format!("'{path}' is not absolute"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        let (leaf, dirs) = parts.split_last().ok_or_else(|| format!("'{path}' names no file"))?;
        let root = openat(libc::AT_FDCWD, &c("/")?, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(|e| format!("cannot open /: {e}"))?;
        let cur = open_dirs(root.as_raw_fd(), dirs, "")?;
        Ok((cur, c(leaf)?, path.to_string()))
    }

    /// The parts of `rel`, a path beneath a root: not absolute, and no part empty, `.`
    /// or `..`, so it can only name something beneath the root. "" is the root itself.
    fn parts(rel: &str) -> Result<Vec<&str>, String> {
        if rel.is_empty() {
            return Ok(Vec::new());
        }
        if rel.starts_with('/') {
            return Err(format!("'{rel}' is absolute; a path here is beneath the folder opened"));
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
            return Err(format!("'{rel}' has an empty, '.' or '..' part; a path here names only what is beneath the folder opened"));
        }
        Ok(parts)
    }

    /// A directory the caller opened, and everything beneath it.
    #[derive(Debug)]
    pub struct Root {
        dir: OwnedFd,
        shown: String,
    }

    /// What one entry of a directory is, its link never followed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        File,
        Dir,
        Link,
        /// A FIFO, socket or device.
        Other,
    }

    /// One entry of a directory.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Entry {
        /// Its name; None when the name is not UTF-8, so no path here can name it.
        pub name: Option<String>,
        pub kind: Kind,
    }

    impl Root {
        /// Opens the directory at `path`: the one place a path is resolved by the
        /// system, as the caller names it (links in it followed, as any program opening
        /// a folder it was given). Everything after goes through the handle.
        pub fn open(path: &std::path::Path) -> Result<Root, String> {
            let shown = path.display().to_string();
            let p = c(path.to_str().ok_or_else(|| format!("'{shown}' is not UTF-8"))?)?;
            let dir = openat(libc::AT_FDCWD, &p, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(|e| match e.raw_os_error() {
                Some(libc::ENOENT) => format!("'{shown}' does not exist"),
                Some(libc::ENOTDIR) => format!("'{shown}' is not a directory"),
                _ => format!("cannot open '{shown}': {e}"),
            })?;
            Ok(Root { dir, shown })
        }

        /// A root from a directory the caller has already opened; `shown` is how it is
        /// named in messages.
        pub fn from_fd(dir: OwnedFd, shown: &str) -> Result<Root, String> {
            // SAFETY: `st` is written by fstat before it is read, and only if it succeeds.
            let st = unsafe {
                let mut st: libc::stat = std::mem::zeroed();
                if libc::fstat(dir.as_raw_fd(), &mut st) != 0 {
                    return Err(format!("'{shown}': {}", errno()));
                }
                st
            };
            if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(format!("'{shown}' is not a directory"));
            }
            Ok(Root { dir, shown: shown.to_string() })
        }

        /// How the folder is named in messages: as it was given to `open` or `from_fd`.
        pub fn shown(&self) -> &str {
            &self.shown
        }

        /// The directory holding `rel`'s last part, that part, and how `rel` is shown.
        fn parent(&self, rel: &str) -> Result<(OwnedFd, CString, String), String> {
            let parts = parts(rel)?;
            let (leaf, dirs) = parts.split_last().ok_or("a path naming a file is needed, not the folder opened itself")?;
            let dir = open_dirs(self.dir.as_raw_fd(), dirs, &self.shown)?;
            Ok((dir, c(leaf)?, format!("{}/{rel}", self.shown)))
        }

        /// Reads the UTF-8 text file at `rel`, at most `max` bytes: a regular file with
        /// one name, as `read` reads.
        pub fn read(&self, rel: &str, max: usize) -> Result<String, String> {
            let (dir, name, shown) = self.parent(rel)?;
            read_in(&dir, &name, &shown, max)
        }

        /// Replaces the file at `rel` with `content`, atomically, as `write` does.
        pub fn write(&self, rel: &str, content: &[u8]) -> Result<(), String> {
            let (dir, name, shown) = self.parent(rel)?;
            write_in(&dir, &name, &shown, content)
        }

        /// The entries of the directory at `rel` ("" for the root), sorted by name, `.`
        /// and `..` left out. No link is followed, in the path or in an entry.
        pub fn list(&self, rel: &str) -> Result<Vec<Entry>, String> {
            let parts = parts(rel)?;
            let dir = open_dirs(self.dir.as_raw_fd(), &parts, &self.shown)?;
            let shown = if rel.is_empty() { self.shown.clone() } else { format!("{}/{rel}", self.shown) };
            list_in(dir, &shown)
        }
    }

    /// Sets errno to 0, so a null from readdir can be told apart: the end, or an error.
    fn clear_errno() {
        // SAFETY: writing this thread's errno through the pointer libc gives for it.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "macos")]
            {
                *libc::__error() = 0;
            }
        }
    }

    fn list_in(dir: OwnedFd, shown: &str) -> Result<Vec<Entry>, String> {
        // A second handle for fstatat: fdopendir takes the first.
        let at = openat(dir.as_raw_fd(), &c(".")?, libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(|e| format!("'{shown}': {e}"))?;
        let raw = std::os::fd::IntoRawFd::into_raw_fd(dir);
        // SAFETY: `raw` is a directory descriptor we own; on success the DIR owns it.
        let d = unsafe { libc::fdopendir(raw) };
        if d.is_null() {
            let e = errno();
            // SAFETY: fdopendir failed, so `raw` is still ours to close.
            unsafe { libc::close(raw) };
            return Err(format!("'{shown}': {e}"));
        }
        struct Closed(*mut libc::DIR);
        impl Drop for Closed {
            fn drop(&mut self) {
                // SAFETY: closing the DIR fdopendir returned, once.
                unsafe { libc::closedir(self.0) };
            }
        }
        let d = Closed(d);
        let mut out = Vec::new();
        loop {
            clear_errno();
            // SAFETY: reading the next entry of a DIR we own; the entry is copied out
            // before the next call.
            let ent = unsafe { libc::readdir(d.0) };
            if ent.is_null() {
                let e = errno();
                if e.raw_os_error().unwrap_or(0) != 0 {
                    return Err(format!("'{shown}': {e}"));
                }
                break;
            }
            // SAFETY: d_name is NUL-terminated within the entry readdir returned.
            let bytes = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes().to_vec();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let cname = CString::new(bytes.clone()).map_err(|_| format!("'{shown}': a name with a NUL"))?;
            let kind = match stat_at(at.as_raw_fd(), &cname) {
                Some(st) if is_link(&st) => Kind::Link,
                Some(st) if is_reg(&st) => Kind::File,
                Some(st) if st.st_mode & libc::S_IFMT == libc::S_IFDIR => Kind::Dir,
                Some(_) => Kind::Other,
                // Gone since it was listed.
                None => continue,
            };
            out.push(Entry { name: String::from_utf8(bytes).ok(), kind });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn read(path: &str, max: usize) -> Result<String, String> {
        let (dir, name, shown) = open_parent(path)?;
        read_in(&dir, &name, &shown, max)
    }

    fn read_in(dir: &OwnedFd, name: &CString, shown: &str, max: usize) -> Result<String, String> {
        let fd = openat(dir.as_raw_fd(), name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0)
            .map_err(|e| open_error(dir.as_raw_fd(), name, shown, e))?;
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

    /// For SQLite, which opens its files itself, by path: checks the database at
    /// canonical `path` as every `fs` path is checked (each folder opened in turn, no
    /// link followed anywhere), that it is a regular file with one name (or absent, when
    /// `create`), and that no link stands where SQLite would put its journal. SQLite
    /// then opens the same path refusing any link in it (SQLITE_OPEN_NOFOLLOW).
    #[cfg(target_os = "linux")]
    pub fn database_check(path: &str, create: bool) -> Result<(), String> {
        let (dir, name, shown) = open_parent(path)?;
        let leaf = name.to_str().map_err(|_| format!("'{shown}' is not a valid name"))?.to_string();
        match stat_at(dir.as_raw_fd(), &name) {
            Some(st) if is_link(&st) => return Err(format!("'{shown}' is a symbolic link; links are never followed")),
            Some(st) if !is_reg(&st) => return Err(format!("'{shown}' is not a regular file")),
            Some(st) if st.st_nlink > 1 => {
                return Err(format!(
                    "'{shown}' has {} hard links; a file with other names could be a file outside the allowed folders",
                    st.st_nlink
                ));
            }
            Some(_) => {}
            None if create => {}
            None => return Err(format!("'{shown}' does not exist")),
        }
        for suffix in ["-journal", "-wal", "-shm"] {
            let side = c(&format!("{leaf}{suffix}"))?;
            if stat_at(dir.as_raw_fd(), &side).is_some_and(|st| is_link(&st)) {
                return Err(format!("'{shown}{suffix}' is a symbolic link; links are never followed"));
            }
        }
        Ok(())
    }

    pub fn write(path: &str, content: &[u8]) -> Result<(), String> {
        let (dir, name, shown) = open_parent(path)?;
        write_in(&dir, &name, &shown, content)
    }

    fn write_in(dir: &OwnedFd, name: &CString, shown: &str, content: &[u8]) -> Result<(), String> {
        let leaf = name.to_str().map_err(|_| format!("'{shown}' is not a valid name"))?.to_string();
        // An existing target must be a regular file; keep its permissions.
        let mode = match stat_at(dir.as_raw_fd(), name) {
            Some(st) if is_link(&st) => return Err(format!("'{shown}' is a symbolic link; links are never followed")),
            Some(st) if !is_reg(&st) => return Err(format!("'{shown}' exists and is not a regular file")),
            Some(st) => Some(st.st_mode & 0o7777),
            None => None,
        };
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).map_err(|e| format!("no randomness for a temporary name: {e}"))?;
        let hex: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
        let tmp_name = format!(".{leaf}.{hex}.staal-tmp");
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
        .map_err(|e| open_error(dir.as_raw_fd(), &tmp, shown, e))?;
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

#[cfg(target_os = "linux")]
pub use imp::database_check;
#[cfg(unix)]
pub use imp::{Entry, Kind, Root};

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
        let d = std::env::temp_dir().join(format!("staalc-fsx-{name}-{}", std::process::id()));
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
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        let d = dir("race");
        let store = d.join("allowed/.store");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("secret"), "ALLOWED").unwrap();
        fs::write(d.join("outside/secret"), "SECRET").unwrap();
        let sw = d.join("allowed/sw");
        let stop = Arc::new(AtomicBool::new(false));
        let rounds = Arc::new(AtomicU64::new(0));
        let swapper = {
            let (stop, rounds, d, sw, store) = (stop.clone(), rounds.clone(), d.clone(), sw.clone(), store.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    fs::rename(&store, &sw).unwrap(); // a real folder
                    fs::rename(&sw, &store).unwrap();
                    symlink(d.join("outside"), &sw).unwrap(); // a link to the outside
                    fs::remove_file(&sw).unwrap();
                    rounds.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let target = s(&sw.join("secret"));
        let (mut allowed, mut refused) = (0u64, 0u64);
        // Reads go on until the race has really been run (many swaps, and reads that met
        // both states), however fast this machine's threads are scheduled; a cap on time
        // keeps a machine that never interleaves them from hanging the test.
        let started = std::time::Instant::now();
        let mut reads = 0u64;
        while reads < 20_000 || rounds.load(Ordering::SeqCst) <= 100 || refused == 0 || allowed == 0 {
            if started.elapsed() > std::time::Duration::from_secs(60) {
                break;
            }
            reads += 1;
            match read(&target, 100) {
                Ok(text) => {
                    assert_eq!(text, "ALLOWED", "a read through a swapped folder leaked");
                    allowed += 1;
                }
                Err(_) => refused += 1,
            }
        }
        stop.store(true, Ordering::SeqCst);
        swapper.join().unwrap();
        let rounds = rounds.load(Ordering::SeqCst);
        assert!(rounds > 100, "the swapper only ran {rounds} rounds in 60 s");
        assert!(refused > 0, "no read met the link ({allowed} allowed)");
        println!("{rounds} swaps; {allowed} reads of the real folder, {refused} refused");
    }

    fn root(d: &Path) -> Root {
        Root::open(&d.join("allowed")).unwrap()
    }

    #[test]
    fn a_root_reads_writes_and_lists_beneath_its_folder() {
        use std::os::unix::ffi::OsStrExt;
        let d = dir("root-basic");
        let r = root(&d);
        fs::create_dir_all(d.join("allowed/src/deep")).unwrap();
        r.write("src/deep/a.staal", b"fn main() -> Int { return 1 }\n").unwrap();
        assert_eq!(r.read("src/deep/a.staal", 100).unwrap(), "fn main() -> Int { return 1 }\n");
        assert_eq!(fs::read_to_string(d.join("allowed/src/deep/a.staal")).unwrap(), "fn main() -> Int { return 1 }\n");
        symlink(d.join("outside"), d.join("allowed/out")).unwrap();
        // SAFETY: creating a FIFO at a test path.
        assert_eq!(unsafe { libc::mkfifo(c_path(&d.join("allowed/pipe")).as_ptr(), 0o600) }, 0);
        // A name that is not UTF-8 (macOS's file systems refuse to create one).
        let odd = cfg!(target_os = "linux");
        if odd {
            fs::write(d.join("allowed").join(std::ffi::OsStr::from_bytes(b"bad\xffname")), "x").unwrap();
        }
        let got = r.list("").unwrap();
        let unnamed: &[Entry] = if odd { &[Entry { name: None, kind: Kind::File }] } else { &[] };
        assert_eq!(&got[..unnamed.len()], unnamed);
        assert_eq!(
            got[unnamed.len()..].to_vec(),
            vec![
                Entry { name: Some("out".into()), kind: Kind::Link },
                Entry { name: Some("pipe".into()), kind: Kind::Other },
                Entry { name: Some("src".into()), kind: Kind::Dir },
            ]
        );
        assert_eq!(r.list("src/deep").unwrap(), vec![Entry { name: Some("a.staal".into()), kind: Kind::File }]);
        assert!(r.read("missing", 100).unwrap_err().contains("does not exist"));
        assert!(r.read("pipe", 100).unwrap_err().contains("not a regular file"));
        assert!(r.list("src/deep/a.staal").unwrap_err().contains("not a directory"));
    }

    #[test]
    fn a_root_refuses_every_path_that_could_name_something_outside() {
        let d = dir("root-paths");
        fs::create_dir_all(d.join("allowed/a")).unwrap();
        fs::write(d.join("allowed/a/f"), "inside").unwrap();
        let r = root(&d);
        for bad in ["..", "../outside/secret", "a/../../outside/secret", "a/..", ".", "./a/f", "a/./f", "/etc/passwd", "a//f", "a/f/", "/"] {
            let e = r.read(bad, 100).unwrap_err();
            assert!(e.contains("absolute") || e.contains("'..'"), "{bad}: {e}");
            assert!(r.write(bad, b"x").is_err(), "{bad}");
            assert!(r.list(bad).is_err(), "{bad}");
        }
        assert!(r.read("", 100).unwrap_err().contains("not the folder opened itself"));
        assert!(r.write("", b"x").is_err());
        assert_eq!(fs::read_to_string(d.join("outside/secret")).unwrap(), "SECRET");
        assert_eq!(r.read("a/f", 100).unwrap(), "inside");
    }

    #[test]
    fn a_root_never_follows_a_link_out() {
        let d = dir("root-links");
        let r = root(&d);
        symlink(d.join("outside"), d.join("allowed/dir")).unwrap();
        symlink(d.join("outside/secret"), d.join("allowed/file")).unwrap();
        assert!(r.read("dir/secret", 100).unwrap_err().contains("symbolic link"));
        assert!(r.write("dir/new", b"x").unwrap_err().contains("symbolic link"));
        assert!(r.list("dir").unwrap_err().contains("symbolic link"));
        assert!(r.read("file", 100).unwrap_err().contains("symbolic link"));
        assert!(r.write("file", b"overwrite").unwrap_err().contains("symbolic link"));
        assert!(!d.join("outside/new").exists());
        assert_eq!(fs::read_to_string(d.join("outside/secret")).unwrap(), "SECRET");
        fs::hard_link(d.join("outside/secret"), d.join("allowed/hard")).unwrap();
        assert!(r.read("hard", 100).unwrap_err().contains("hard links"));
    }

    #[test]
    fn a_root_stays_its_folder_when_the_folder_is_moved_and_its_name_made_a_link() {
        let d = dir("root-moved");
        fs::write(d.join("allowed/f"), "INSIDE").unwrap();
        fs::write(d.join("outside/f"), "OUTSIDE").unwrap();
        let r = root(&d);
        fs::rename(d.join("allowed"), d.join("moved")).unwrap();
        symlink(d.join("outside"), d.join("allowed")).unwrap();
        assert_eq!(r.read("f", 100).unwrap(), "INSIDE");
        r.write("g", b"new").unwrap();
        assert_eq!(fs::read_to_string(d.join("moved/g")).unwrap(), "new");
        assert!(!d.join("outside/g").exists());
        assert_eq!(r.list("").unwrap().iter().filter_map(|e| e.name.clone()).collect::<Vec<_>>(), vec!["f", "g"]);
    }

    #[test]
    fn a_root_is_a_directory() {
        let d = dir("root-kind");
        assert!(Root::open(&d.join("outside/secret")).unwrap_err().contains("not a directory"));
        assert!(Root::open(&d.join("absent")).unwrap_err().contains("does not exist"));
        let f: std::os::fd::OwnedFd = fs::File::open(d.join("outside/secret")).unwrap().into();
        assert!(Root::from_fd(f, "secret").unwrap_err().contains("not a directory"));
        let dirfd: std::os::fd::OwnedFd = fs::File::open(d.join("allowed")).unwrap().into();
        let r = Root::from_fd(dirfd, "the project").unwrap();
        r.write("x", b"1").unwrap();
        assert!(r.read("nope", 10).unwrap_err().contains("'the project/nope' does not exist"));
    }

    /// As `a_folder_swapped_for_a_link_mid_read_never_leaks`, through a root.
    #[test]
    fn a_folder_beneath_a_root_swapped_for_a_link_mid_read_never_leaks() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        let d = dir("root-race");
        let store = d.join("allowed/.store");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("secret"), "ALLOWED").unwrap();
        let sw = d.join("allowed/sw");
        let r = root(&d);
        let stop = Arc::new(AtomicBool::new(false));
        let rounds = Arc::new(AtomicU64::new(0));
        let swapper = {
            let (stop, rounds, d, sw, store) = (stop.clone(), rounds.clone(), d.clone(), sw.clone(), store.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    fs::rename(&store, &sw).unwrap();
                    fs::rename(&sw, &store).unwrap();
                    symlink(d.join("outside"), &sw).unwrap();
                    fs::remove_file(&sw).unwrap();
                    rounds.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let (mut allowed, mut refused, mut reads) = (0u64, 0u64, 0u64);
        let started = std::time::Instant::now();
        while reads < 20_000 || rounds.load(Ordering::SeqCst) <= 100 || refused == 0 || allowed == 0 {
            if started.elapsed() > std::time::Duration::from_secs(60) {
                break;
            }
            reads += 1;
            match r.read("sw/secret", 100) {
                Ok(text) => {
                    assert_eq!(text, "ALLOWED", "a read through a swapped folder leaked");
                    allowed += 1;
                }
                Err(_) => refused += 1,
            }
            match r.list("sw") {
                Ok(entries) => assert!(entries.iter().all(|e| e.name.as_deref() == Some("secret")), "a listing leaked: {entries:?}"),
                Err(_) => refused += 1,
            }
        }
        stop.store(true, Ordering::SeqCst);
        swapper.join().unwrap();
        assert!(rounds.load(Ordering::SeqCst) > 100);
        assert!(refused > 0 && allowed > 0, "{allowed} allowed, {refused} refused");
    }

}
