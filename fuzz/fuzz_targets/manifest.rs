// Package manifests, lockfiles, registry entries, versions and version ranges from
// any text: refused with a message or read, never a panic; a version reads back as
// itself; and a range answers for any version.
#![no_main]
use libfuzzer_sys::fuzz_target;
use staalc::packages::{parse_entries, Lock, Manifest};
use staalc::semver::{Range, Version};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let _ = Manifest::parse(s, "m", true);
    let _ = Manifest::parse(s, "m", false);
    if let Ok(l) = Lock::parse(s, "l") {
        assert_eq!(Lock::parse(&l.to_text(), "l").as_ref(), Ok(&l), "a lock does not read back as itself");
    }
    let _ = parse_entries(s, "p", "e");
    // `range|version`: both sides parsed, and matched when both are valid.
    let (r, v) = s.split_once('|').unwrap_or((s, s));
    if let Ok(v) = Version::parse(v) {
        assert_eq!(Version::parse(&v.to_string()).as_ref(), Ok(&v), "a version does not read back as itself");
        if let Ok(r) = Range::parse(r) {
            let _ = r.matches(&v);
        }
    }
});
