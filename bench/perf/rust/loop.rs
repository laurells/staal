fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let (mut i, mut s) = (0i64, 0i64);
    while i < n {
        s += (i * i) % 7;
        i += 1;
    }
    println!("{s}");
}
