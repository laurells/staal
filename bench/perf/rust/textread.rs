fn main() {
    let n: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    let mut t = String::new();
    for _ in 0..n / 10 {
        t.push_str("abcdéfghij");
    }
    let mut s: i64 = 0;
    for j in 0..1000 {
        s += t.chars().count() as i64;
        if t.contains(&j.to_string()) {
            s += 1;
        }
    }
    println!("{s}");
}
