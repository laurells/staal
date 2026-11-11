// install.sh, run against a local copy of a release: it installs the archive for this
// machine into its own folders, links staalc, and refuses a download whose SHA-256 is
// not the release's, installing nothing. Its check of the build provenance is run
// against a stand-in GitHub CLI.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[path = "../src/test_ports.rs"]
#[allow(dead_code)] // address() is not needed here
mod test_ports;

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
    release(dir, "9.8.7", good_hash);
    serve(dir)
}

/// Release `version` in `dir`/www: its archive, with this build's staalc, and its SHA-256.
fn release(dir: &Path, version: &str, good_hash: bool) {
    let name = format!("staalc-{version}-x86_64-unknown-linux-gnu");
    let stage = dir.join("stage").join(&name);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::copy(BIN, stage.join("staalc")).unwrap();
    let release = dir.join(format!("www/v{version}"));
    std::fs::create_dir_all(&release).unwrap();
    let archive = release.join(format!("{name}.tar.gz"));
    let ok = Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(dir.join("stage")).arg(&name).status().unwrap();
    assert!(ok.success());
    let hash = if good_hash { staalc::packages::sha256_hex(&std::fs::read(&archive).unwrap()) } else { "0".repeat(64) };
    std::fs::write(release.join(format!("{name}.tar.gz.sha256")), format!("{hash}  {name}.tar.gz\n")).unwrap();
}

/// Serves `dir`/www over HTTP on a free port.
fn serve(dir: &Path) -> Mirror {
    let port = test_ports::port();
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

/// Runs install.sh for v9.8.7 from the mirror, with `gh` as its GitHub CLI and `extra`
/// in its environment.
fn install_with(home: &Path, port: u16, gh: &Path, extra: &[(&str, &str)]) -> (Option<i32>, String) {
    install_version(home, port, "9.8.7", gh, extra)
}

fn install_version(home: &Path, port: u16, version: &str, gh: &Path, extra: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut c = Command::new("sh");
    c.arg(Path::new(ROOT).join("install.sh"))
        .env("HOME", home)
        .env("STAAL_VERSION", version)
        .env("STAAL_DOWNLOAD_BASE", format!("http://127.0.0.1:{port}"))
        .env("STAAL_GH", gh)
        .env_remove("STAAL_REQUIRE_PROVENANCE");
    for (k, v) in extra {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (o.status.code(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

/// Without a GitHub CLI.
fn install(home: &Path, port: u16) -> (Option<i32>, String) {
    install_with(home, port, Path::new("/nonexistent/gh"), &[])
}

/// A stand-in GitHub CLI: signed in or not, and `attestation verify` succeeding or not.
/// It writes the arguments of each call to `calls`.
fn fake_gh(dir: &Path, signed_in: bool, verifies: bool) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let gh = dir.join("gh");
    let calls = dir.join("calls");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$1 $2\" in\n  \"auth status\") exit {} ;;\n  \"attestation verify\") {} ;;\nesac\nexit 2\n",
        calls.display(),
        if signed_in { 0 } else { 1 },
        if verifies { "exit 0" } else { "echo 'no attestations found' >&2; exit 1" },
    );
    std::fs::write(&gh, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
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

#[test]
fn install_sh_checks_the_build_provenance_when_it_can() {
    let dir = scratch("provenance");
    let _ = std::fs::remove_dir_all(&dir);
    release(&dir, "9.8.7", true);
    release(&dir, "0.26.0", true);
    let m = serve(&dir);
    let fresh = |n: &str| {
        let h = dir.join(n);
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    };
    let installed = |h: &Path| h.join(".local/bin/staalc").exists() && h.join(".staal/9.8.7").exists();
    let archive = format!("{NAME}.tar.gz");

    // Signed in, and the attestation verifies: installed, and said so. The check names
    // the repository, the release workflow, the tag and GitHub-hosted runners.
    let gh = fake_gh(&dir.join("gh-ok"), true, true);
    let home = fresh("home-ok");
    let (code, out) = install_with(&home, m.1, &gh, &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("build provenance verified"), "{out}");
    assert!(installed(&home));
    let calls = std::fs::read_to_string(dir.join("gh-ok/calls")).unwrap();
    let verify = calls.lines().find(|l| l.starts_with("attestation verify")).expect(&calls);
    assert!(verify.contains(&archive), "{verify}");
    for want in [
        "--repo laurells/staal",
        "--signer-workflow laurells/staal/.github/workflows/release.yml",
        "--source-ref refs/tags/v9.8.7",
        "--deny-self-hosted-runners",
    ] {
        assert!(verify.contains(want), "{want} missing: {verify}");
    }

    // Signed in, and it does not verify, for a release after 0.26.0: refused.
    let gh = fake_gh(&dir.join("gh-bad"), true, false);
    let home = fresh("home-bad");
    let (code, out) = install_with(&home, m.1, &gh, &[]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("does not verify") && out.contains("nothing was installed"), "{out}");
    assert!(!installed(&home));

    // A release from before attestations (0.26.0 and earlier) has none: installed on its
    // SHA-256, and said so; refused if the provenance is required.
    let home = fresh("home-old");
    let (code, out) = install_version(&home, m.1, "0.26.0", &gh, &[]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("released before releases carried a build provenance attestation"), "{out}");
    assert!(home.join(".staal/0.26.0/staalc").exists());
    let home = fresh("home-old-required");
    let (code, out) = install_version(&home, m.1, "0.26.0", &gh, &[("STAAL_REQUIRE_PROVENANCE", "1")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("nothing was installed"), "{out}");
    assert!(!home.join(".staal/0.26.0").exists());

    // No GitHub CLI, or one not signed in: installed on its SHA-256, and said so.
    for (n, gh) in [("home-none", PathBuf::from("/nonexistent/gh")), ("home-out", fake_gh(&dir.join("gh-out"), false, true))] {
        let home = fresh(n);
        let (code, out) = install_with(&home, m.1, &gh, &[]);
        assert_eq!(code, Some(0), "{out}");
        assert!(out.contains("build provenance not checked"), "{out}");
        assert!(installed(&home));
    }
    assert!(!std::fs::read_to_string(dir.join("gh-out/calls")).unwrap().contains("attestation"), "verified while signed out");

    // Unless the provenance is required.
    let home = fresh("home-required");
    let (code, out) = install_with(&home, m.1, Path::new("/nonexistent/gh"), &[("STAAL_REQUIRE_PROVENANCE", "1")]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("STAAL_REQUIRE_PROVENANCE=1") && out.contains("nothing was installed"), "{out}");
    assert!(!installed(&home));
}
