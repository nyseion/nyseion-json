use std::{hint::black_box, time::Instant};
fn main() {
  let path = std::env::args().nth(1).unwrap();
  let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
  let compact = serde_json::to_vec(&v).unwrap();
  let mut out = Vec::with_capacity(compact.len() * 4);
  let t = Instant::now();
  let n = 300;
  for _ in 0..n {
    out.clear();
    nyseion_json::pretty::reformat(black_box(&compact), b"  ", &mut out);
  }
  let ns = t.elapsed().as_nanos() as f64 / n as f64;
  println!(
    "in {} B out {} B: {:.0} MB/s of input, {:.0} MB/s of output",
    compact.len(),
    out.len(),
    compact.len() as f64 / ns * 1e3,
    out.len() as f64 / ns * 1e3
  );
}
