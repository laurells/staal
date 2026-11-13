// Exact money as integer minor units (cents), as Rust code keeps it.
fn main() {
    let n: i64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let price: i64 = 37;
    let mut total: i64 = 0;
    for i in 0..n {
        total += price * (i % 10 + 1);
    }
    println!("\"{}.{:02} USD\"", total / 100, total % 100);
}
