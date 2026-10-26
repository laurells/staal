// install.sh, run against a local copy of a release: it installs the archive for this
// machine into its own folders, links staalc, and refuses a download whose SHA-256 is
// not the release's, installing nothing.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const NAME: &str = "staalc-9.8.7-x86_64-unknown-linux-gnu";

struct Mirror(Child, u16);

impl Drop for Mirror {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A release v9.8.7 holding an archive with this build's staalc, served over HTTP.
fn mirror(dir: &Path, good_hash: bool) -> Mirror {
    let _ = std::fs::remove_dir_all(dir);
    let stage = dir.join("stage").join(NAME);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::copy(BIN, stage.join("staalc")).unwrap();
    let release = dir.join("www/v9.8.7");
    std::fs::create_dir_all(&release).unwrap();
    let archive = release.join(format!("{NAME}.tar.gz"));
    let ok = Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(dir.join("stage")).arg(NAME).status().unwrap();
    assert!(ok.success());
    let hash = if good_hash { staalc::packages::sha256_hex(&std::fs::read(&archive).unwrap()) } else { "0".repeat(64) };
    std::fs::write(release.join(format!("{NAME}.tar.gz.sha256")), format!("{hash}  {NAME}.tar.gz\n")).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new("python3")
        .args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1", "--directory"])
        .arg(dir.join("www"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(start.elapsed().as_secs() < 20, "the mirror did not start");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Mirror(child, port)
}

fn install(home: &Path, port: u16) -> (Option<i32>, String) {
    let o = Command::new("sh")
        .arg(Path::new(ROOT).join("install.sh"))
        .env("HOME", home)
        .env("STAAL_VERSION", "9.8.7")
        .env("STAAL_DOWNLOAD_BASE", format!("http://127.0.0.1:{port}"))
        .output()
        .unwrap();
    (o.status.code(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("install").join(name)
}

#[test]
fn install_sh_installs_a_release_and_refuses_a_bad_download() {
    let dir = scratch("good");
    let m = mirror(&dir, true);
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (code, out) = install(&home, m.1);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&format!("installed: staalc {}", env!("CARGO_PKG_VERSION"))), "{out}");
    let linked = home.join(".local/bin/staalc");
    assert_eq!(std::fs::read_link(&linked).unwrap(), home.join(".staal/9.8.7/staalc"));
    let v = Command::new(&linked).arg("--version").output().unwrap();
    assert!(v.status.success());

    let dir = scratch("bad");
    let m = mirror(&dir, false);
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (code, out) = install(&home, m.1);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("nothing was installed"), "{out}");
    assert!(!home.join(".local/bin/staalc").exists() && !home.join(".staal/9.8.7").exists());

    // A version that is not one is refused before anything is downloaded.
    let o = Command::new("sh").arg(Path::new(ROOT).join("install.sh")).env("HOME", &home).env("STAAL_VERSION", "1.0; rm -rf ~").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("is not a version"));
}
