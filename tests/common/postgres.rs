// A PostgreSQL server for tests: initialised in a scratch folder, listening on
// 127.0.0.1 at a free port with password (SCRAM) authentication, and stopped when
// dropped. Its programs come from STAAL_POSTGRES_BIN, or the newest
// /usr/lib/postgresql/*/bin, or `pg_config --bindir`. Postgres refuses to run as root,
// so as root the server runs as `nobody` (through runuser).
//
// Without a server to run, a test that needs one fails, unless STAAL_SKIP_POSTGRES=1
// says to skip it: a missing server never passes silently.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const USER: &str = "staal";
pub const PASSWORD: &str = "pw-for-tests-7f3a9c";

fn bin_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("STAAL_POSTGRES_BIN") {
        return Some(PathBuf::from(d));
    }
    let mut versions: Vec<PathBuf> = std::fs::read_dir("/usr/lib/postgresql")
        .map(|r| r.flatten().map(|e| e.path().join("bin")).filter(|p| p.join("postgres").exists()).collect())
        .unwrap_or_default();
    versions.sort_by_key(|p| p.parent().and_then(|v| v.file_name()).and_then(|v| v.to_str()).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0));
    if let Some(p) = versions.pop() {
        return Some(p);
    }
    let o = Command::new("pg_config").arg("--bindir").output().ok()?;
    let d = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    d.join("postgres").exists().then_some(d)
}

/// Whether to skip, saying so, because there is no server and the environment asks to.
pub fn skip() -> bool {
    if bin_dir().is_some() {
        return false;
    }
    if std::env::var("STAAL_SKIP_POSTGRES").as_deref() == Ok("1") {
        eprintln!("skipped: no PostgreSQL server (STAAL_SKIP_POSTGRES=1)");
        return true;
    }
    panic!("these tests need a PostgreSQL server: install it, or set STAAL_POSTGRES_BIN to its bin folder, or STAAL_SKIP_POSTGRES=1 to skip them");
}

fn root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// A command run as the server's user.
fn as_server(program: &Path) -> Command {
    if root() {
        let mut c = Command::new("runuser");
        c.args(["-u", "nobody", "--"]).arg(program);
        c
    } else {
        Command::new(program)
    }
}

fn give_to_server(path: &Path) {
    if root() {
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: a valid path; 65534 is nobody on Debian, Ubuntu and their kin.
        let r = unsafe { libc::chown(c.as_ptr(), 65534, 65534) };
        assert_eq!(r, 0, "chown {}", path.display());
    }
}

pub struct Server {
    pub port: u16,
    pub dir: PathBuf,
    bin: PathBuf,
    child: Child,
}

impl Server {
    /// A server in `dir`, with TLS from `tls` (certificate and key in PEM) if given.
    pub fn start(dir: &Path, tls: Option<(&str, &str)>) -> Server {
        let bin = bin_dir().expect("checked by skip()");
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let data = dir.join("data");
        let pwfile = dir.join("pw");
        std::fs::write(&pwfile, PASSWORD).unwrap();
        for p in [dir, &pwfile] {
            give_to_server(p);
        }
        let o = as_server(&bin.join("initdb"))
            .args(["-D", data.to_str().unwrap(), "-U", USER, "--auth=scram-sha-256", "-E", "UTF8", "--locale=C", "--pwfile", pwfile.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(o.status.success(), "initdb: {}", String::from_utf8_lossy(&o.stderr));
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut args = vec![
            "-D".to_string(),
            data.to_str().unwrap().to_string(),
            "-p".into(),
            port.to_string(),
            "-c".into(),
            "listen_addresses=127.0.0.1".into(),
            "-c".into(),
            format!("unix_socket_directories={}", dir.display()),
            "-c".into(),
            "fsync=off".into(),
        ];
        if let Some((cert, key)) = tls {
            let (c, k) = (data.join("server.crt"), data.join("server.key"));
            std::fs::write(&c, cert).unwrap();
            std::fs::write(&k, key).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&k, std::fs::Permissions::from_mode(0o600)).unwrap();
            give_to_server(&c);
            give_to_server(&k);
            args.extend(["-c".into(), "ssl=on".into()]);
        }
        let child = as_server(&bin.join("postgres")).args(&args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let server = Server { port, dir: dir.to_path_buf(), bin, child };
        let start = std::time::Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(start.elapsed().as_secs() < 30, "postgres did not start");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Listening is not yet accepting: wait until a query works.
        while server.psql("postgres", "SELECT 1").is_err() {
            assert!(start.elapsed().as_secs() < 30, "postgres did not become ready");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        server
    }

    /// Runs SQL with psql, as the test user, giving its output (unaligned, no headers).
    pub fn psql(&self, db: &str, sql: &str) -> Result<String, String> {
        let o = Command::new(self.bin.join("psql"))
            .args(["-h", "127.0.0.1", "-p", &self.port.to_string(), "-U", USER, "-d", db, "-At", "-v", "ON_ERROR_STOP=1", "-c", sql])
            .env("PGPASSWORD", PASSWORD)
            .env("PGSSLMODE", "prefer")
            .output()
            .map_err(|e| e.to_string())?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // An immediate shutdown, which also ends the server's own processes; then the
        // process started (runuser, or the server itself) is reaped.
        let data = self.dir.join("data");
        let _ = as_server(&self.bin.join("pg_ctl")).args(["stop", "-D", data.to_str().unwrap(), "-m", "immediate", "-w"]).output();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
