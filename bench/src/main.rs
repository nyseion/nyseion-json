//! Serialization throughput: serde_json and sonic-rs (derive) against rimu-json's derive and a
//! hand-written `JsonWriter`. Usage: bench <api.json> <text-file>
use rimu_json::{JsonWrite, JsonWriter};
use serde::{Deserialize, Serialize};
use std::{hint::black_box, time::Instant};

/// Median ns per call over 9 batches of ~30 ms.
fn time(mut f: impl FnMut() -> usize) -> f64 {
  let t = Instant::now();
  let mut n = 0u64;
  while t.elapsed().as_millis() < 30 {
    black_box(f());
    n += 1;
  }
  let iters = ((30e6 / (t.elapsed().as_nanos() as f64 / n as f64)) as u64).max(1);
  let mut samples: Vec<f64> = (0..9)
    .map(|_| {
      let t = Instant::now();
      for _ in 0..iters {
        black_box(f());
      }
      t.elapsed().as_nanos() as f64 / iters as f64
    })
    .collect();
  samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
  samples[4]
}

#[derive(Serialize, Deserialize, JsonWrite)]
struct Rating {
  score: i64,
  count: i64,
}
#[derive(Serialize, Deserialize, JsonWrite)]
struct Item {
  id: i64,
  name: String,
  category: String,
  price: i64,
  quantity: i64,
  active: bool,
  tags: Vec<String>,
  rating: Rating,
}
#[derive(Serialize, JsonWrite)]
struct Point {
  lat: f64,
  lon: f64,
  alt: f64,
}
#[derive(Serialize, JsonWrite)]
struct Post {
  id: u64,
  user: String,
  text: String,
  lang: String,
  retweets: u64,
}
#[derive(Serialize, JsonWrite)]
#[serde(rename_all = "camelCase")]
struct Order {
  order_id: u64,
  customer_name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  coupon: Option<String>,
  total: f64,
  status: Status,
  lines: Vec<Line>,
}
#[derive(Serialize, JsonWrite)]
#[serde(rename_all = "snake_case")]
enum Status {
  Pending,
  Shipped { carrier: String, tracking: String },
}
#[derive(Serialize, JsonWrite)]
struct Line {
  sku: String,
  qty: u32,
  unit_price: f64,
}

fn hand_items(w: &mut JsonWriter<'_>, items: &[Item]) {
  w.raw("[");
  for el in items {
    w.raw("{\"id\":");
    w.i64(el.id);
    w.raw(",\"name\":\"");
    w.str_contents(&el.name);
    w.raw("\",\"category\":\"");
    w.str_contents(&el.category);
    w.raw("\",\"price\":");
    w.i64(el.price);
    w.raw(",\"quantity\":");
    w.i64(el.quantity);
    w.raw(",\"active\":");
    w.bool(el.active);
    w.raw(",\"tags\":[");
    for t in &el.tags {
      w.raw("\"");
      w.str_contents(t);
      w.raw("\",");
    }
    w.end_seq(b']');
    w.raw(",\"rating\":{\"score\":");
    w.i64(el.rating.score);
    w.raw(",\"count\":");
    w.i64(el.rating.count);
    w.raw("}},");
  }
  w.end_seq(b']');
}

struct Rng(u64);
impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
  fn f(&mut self) -> f64 {
    (self.next() >> 11) as f64 / (1u64 << 53) as f64
  }
}

fn bench<T: Serialize + JsonWrite + ?Sized>(
  name: &str,
  value: &T,
  hand: Option<&dyn Fn(&mut JsonWriter<'_>)>,
) {
  let reference = serde_json::to_vec(value).unwrap();
  assert_eq!(sonic_rs::to_vec(value).unwrap(), reference, "sonic-rs differs on {name}");
  assert_eq!(rimu_json::to_vec(value), reference, "rimu derive differs on {name}");
  let len = reference.len();
  let mut v = Vec::with_capacity(len * 2);
  let serde = time(|| {
    v.clear();
    serde_json::to_writer(&mut v, value).unwrap();
    v.len()
  });
  let sonic = time(|| {
    v.clear();
    sonic_rs::to_writer(&mut v, value).unwrap();
    v.len()
  });
  let derive = time(|| {
    v.clear();
    rimu_json::write_to(value, &mut v);
    v.len()
  });
  let hand = hand.map(|f| {
    v.clear();
    f(&mut JsonWriter::new(&mut v));
    assert_eq!(v, reference, "hand-written differs on {name}");
    time(|| {
      v.clear();
      f(&mut JsonWriter::new(&mut v));
      v.len()
    })
  });
  let mbs = |ns: f64| len as f64 / ns * 1e3;
  println!(
    "| {name} | {} | {:.0} | {:.0} | **{:.0}** | {} | {:.2}× | {:.2}× |",
    if len >= 10_000 { format!("{} KB", len / 1000) } else { format!("{len} B") },
    mbs(serde),
    mbs(sonic),
    mbs(derive),
    hand.map_or("–".into(), |ns| format!("{:.0}", mbs(ns))),
    serde / derive,
    sonic / derive,
  );
}

fn main() {
  let mut args = std::env::args().skip(1);
  let items: Vec<Item> =
    serde_json::from_slice(&std::fs::read(args.next().expect("api.json")).unwrap()).unwrap();
  let text = std::fs::read_to_string(args.next().expect("text file")).unwrap();
  let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
  let points: Vec<Point> = (0..20_000)
    .map(|_| Point {
      lat: rng.f() * 180.0 - 90.0,
      lon: rng.f() * 360.0 - 180.0,
      alt: (rng.f() * 100_000.0).round() / 100.0,
    })
    .collect();
  let chars: Vec<char> = text.chars().collect();
  let extras = ['"', '\n', '\t', '\\', 'é', '日', '😀'];
  let posts: Vec<Post> = (0..5_000)
    .map(|i| {
      let start = (rng.next() as usize) % (chars.len() - 300);
      let n = 40 + (rng.next() as usize) % 240;
      let mut s: String = chars[start..start + n].iter().collect();
      if rng.next().is_multiple_of(3) {
        s.push(extras[(rng.next() as usize) % extras.len()]);
      }
      Post {
        id: 1_000_000_000_000 + i,
        user: format!("user_{}", rng.next() % 100_000),
        text: s,
        lang: "en".into(),
        retweets: rng.next() % 10_000,
      }
    })
    .collect();
  let orders: Vec<Order> = (0..2_000)
    .map(|i| Order {
      order_id: 10_000_000 + i,
      customer_name: format!("Customer {}", rng.next() % 50_000),
      coupon: rng.next().is_multiple_of(4).then(|| format!("SAVE{}", rng.next() % 50)),
      total: (rng.next() % 100_000) as f64 / 100.0,
      status: if rng.next().is_multiple_of(2) {
        Status::Pending
      } else {
        Status::Shipped { carrier: "UPS".into(), tracking: format!("1Z{:016X}", rng.next()) }
      },
      lines: (0..1 + rng.next() % 5)
        .map(|_| Line {
          sku: format!("SKU-{:05}", rng.next() % 100_000),
          qty: 1 + (rng.next() % 9) as u32,
          unit_price: (rng.next() % 20_000) as f64 / 100.0,
        })
        .collect(),
    })
    .collect();
  println!("escape kernel: {:?}", rimu_json::kernel());
  println!();
  println!(
    "| data | output | serde_json MB/s | sonic-rs MB/s | rimu derive MB/s | rimu hand MB/s | derive vs serde | derive vs sonic |"
  );
  println!("|---|---|---|---|---|---|---|---|");
  for n in [10, items.len()] {
    let slice = &items[..n];
    bench(&format!("API items ×{n}"), slice, Some(&|w| hand_items(w, slice)));
  }
  for n in [10, points.len()] {
    bench(&format!("float points ×{n}"), &points[..n], None);
  }
  for n in [10, posts.len()] {
    bench(&format!("text posts ×{n}"), &posts[..n], None);
  }
  for n in [10, orders.len()] {
    bench(&format!("orders (enum, rename, skip_if) ×{n}"), &orders[..n], None);
  }
}
