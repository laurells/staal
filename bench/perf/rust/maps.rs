use std::collections::HashMap;

fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let mut counts: HashMap<i64, i64> = HashMap::new();
    counts.insert(0, 0);
    for i in 0..n {
        *counts.entry((i * 7919) % 1000).or_insert(0) += 1;
    }
    let s: i64 = counts.iter().map(|(k, v)| k * v).sum();
    println!("{s}");
}
