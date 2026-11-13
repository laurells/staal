fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let mut xs: Vec<i64> = Vec::new();
    let mut x: i64 = 12345;
    for _ in 0..n {
        x = (x * 1103515245 + 12345) % 2147483648;
        xs.push(x);
    }
    xs.sort();
    let l = xs.len();
    println!("{}", xs[0] + xs[l / 2] + xs[l - 1]);
}
