// The formatter on any text: it either refuses, or gives text that formats to itself
// and that the checker judges exactly as it judged the original.
#![no_main]
use libfuzzer_sys::fuzz_target;

fn codes(src: &str) -> Vec<&'static str> {
    staalc::compile_check(src).iter().map(|d| d.code).collect()
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let Ok(out) = staalc::fmt::format(s) else { return };
    assert_eq!(staalc::fmt::format(&out).as_deref(), Ok(out.as_str()), "formatting is not stable");
    assert_eq!(codes(s), codes(&out), "formatting changed what the checker says");
});
