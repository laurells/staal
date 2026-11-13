use std::collections::HashMap;

fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let mut m: HashMap<i64, i64> = HashMap::new();
    m.insert(0, 0);
    for i in 1..n {
        m.insert((i * 7919) % 1000003, i);
    }
    let mut s: i64 = 0;
    for j in 0..n {
        match m.get(&((j * 7919) % 1000003)) {
            Some(v) => s += v,
            None => s -= 1,
        }
    }
    println!("{s}");
}
