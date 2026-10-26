// Any text, given to the checker as a program: lexing, parsing, modules, aliases,
// constants, type and trust checks and budgets must refuse or accept it, never panic
// or hang.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = staalc::compile_check(s);
    }
});
