// docs/TUTORIAL.md, followed exactly as a newcomer would: in an empty folder, every
// file it says to save is saved, and every command in a `console` block is run, in
// order, and must print exactly what the tutorial shows (standard output, then
// standard error). The phase 13 gate is that a developer gets from install to a
// working program without help; this keeps the tutorial true.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

#[test]
fn the_tutorial_works_as_written() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/TUTORIAL.md")).unwrap();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tutorial");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut lines = text.lines();
    let (mut files, mut commands) = (0, 0);
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```") else { continue };
        let mut block = Vec::new();
        for l in lines.by_ref() {
            if l == "```" {
                break;
            }
            block.push(l);
        }
        if let Some(name) = info.split_whitespace().find_map(|w| w.strip_prefix("file=")) {
            std::fs::write(dir.join(name), block.join("\n") + "\n").unwrap();
            files += 1;
        } else if info == "console" {
            let mut i = 0;
            while i < block.len() {
                let cmd = block[i].strip_prefix("$ ").unwrap_or_else(|| panic!("a console block starts with a command: {}", block[i]));
                let mut want = Vec::new();
                i += 1;
                while i < block.len() && !block[i].starts_with("$ ") {
                    want.push(block[i]);
                    i += 1;
                }
                let words: Vec<&str> = cmd.split_whitespace().collect();
                assert_eq!(words[0], "staalc", "{cmd}");
                let o = Command::new(BIN).args(&words[1..]).current_dir(&dir).output().unwrap();
                let got = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                assert_eq!(got.trim_end(), want.join("\n"), "`{cmd}` printed something else");
                commands += 1;
            }
        }
    }
    // Every program and config the tutorial has, and every command, was used.
    assert_eq!((files, commands), (13, 17));
}
