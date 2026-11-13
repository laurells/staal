fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let mut t = String::new();
    for i in 0..n {
        t.push_str(&i.to_string());
        t.push(',');
    }
    println!("{}", t.len());
}
