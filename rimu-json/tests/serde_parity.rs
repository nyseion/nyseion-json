//! Every value is written by `rimu_json` and by `serde_json` and the bytes must be equal.

use rimu_json::{JsonWrite, JsonWriter};
use serde::Serialize;
use std::{
  borrow::Cow,
  collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
  rc::Rc,
  sync::Arc,
};

#[track_caller]
fn same<T: Serialize + JsonWrite + ?Sized>(value: &T) {
  let expected = serde_json::to_string(value).unwrap();
  assert_eq!(rimu_json::to_string(value), expected);
  // Appending keeps what was there.
  let mut buf = b"x".to_vec();
  rimu_json::write_to(value, &mut buf);
  assert_eq!(&buf[1..], expected.as_bytes());
  // Streaming to an `io::Write`.
  let mut out = Vec::new();
  rimu_json::to_writer(&mut out, value).unwrap();
  assert_eq!(out, expected.as_bytes());
  // Pretty, in memory and streaming.
  let pretty = serde_json::to_string_pretty(value).unwrap();
  assert_eq!(rimu_json::to_string_pretty(value), pretty);
  let mut out = Vec::new();
  rimu_json::to_writer_pretty(&mut out, value).unwrap();
  assert_eq!(out, pretty.as_bytes());
}

struct Rng(u64);
impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
}

#[derive(Serialize, JsonWrite)]
struct Rating {
  score: i64,
  count: u32,
}

#[derive(Serialize, JsonWrite)]
struct Item<'a> {
  id: u64,
  name: String,
  category: &'a str,
  price: f64,
  active: bool,
  tags: Vec<&'a str>,
  rating: Rating,
  note: Option<String>,
}

#[derive(Serialize, JsonWrite)]
#[serde(rename_all = "camelCase")]
struct Renamed {
  first_name: &'static str,
  #[serde(rename = "LAST \"name\"\n")]
  last_name: String,
  #[serde(skip)]
  #[allow(dead_code)]
  secret: u8,
  #[serde(skip_serializing)]
  #[allow(dead_code)]
  hidden: u8,
  r#type: u8,
  user_id_2: u8,
}

#[derive(Serialize, JsonWrite)]
struct Skips {
  #[serde(skip_serializing_if = "Option::is_none")]
  a: Option<u8>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  b: Vec<u8>,
  c: u8,
  #[serde(skip_serializing_if = "Option::is_none")]
  d: Option<&'static str>,
  e: &'static str,
}

#[derive(Serialize, JsonWrite)]
struct AllOptional {
  #[serde(skip_serializing_if = "Option::is_none")]
  a: Option<u8>,
  #[serde(skip_serializing_if = "Option::is_none")]
  b: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  c: Option<bool>,
}

#[derive(Serialize, JsonWrite)]
struct Empty {}
#[derive(Serialize, JsonWrite)]
struct Unit;
#[derive(Serialize, JsonWrite)]
struct Newtype(String);
#[derive(Serialize, JsonWrite)]
#[allow(dead_code)]
struct Tuple(u8, &'static str, #[serde(skip)] u8, f32);
#[derive(Serialize, JsonWrite)]
#[serde(transparent)]
struct Transparent {
  inner: Vec<u8>,
}

#[derive(Serialize, JsonWrite)]
struct Generic<T, U: Clone> {
  value: T,
  list: Vec<U>,
}

#[derive(Serialize, JsonWrite)]
#[serde(rename_all = "snake_case", rename_all_fields = "UPPERCASE")]
enum Event<'a> {
  Started,
  #[serde(rename = "stopped!")]
  Stopped,
  Message(&'a str),
  Moved(i32, i32),
  Nothing(),
  LoggedIn {
    user_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<u64>,
  },
  #[serde(rename_all = "kebab-case")]
  LoggedOut {
    user_name: String,
    #[serde(skip)]
    #[allow(dead_code)]
    reason: u8,
  },
  Wrapped(Box<Event<'a>>),
}

#[derive(Serialize, JsonWrite)]
enum Never {}

#[test]
fn structs() {
  let item = Item {
    id: 1,
    name: "Alpha \"Widget\"\n".into(),
    category: "tools",
    price: 19.99,
    active: true,
    tags: vec!["sale", "a\\b", ""],
    rating: Rating { score: -3, count: 7 },
    note: None,
  };
  same(&item);
  same(&Item { tags: vec![], note: Some("é日😀\u{1}".into()), price: 1e300, ..item });
  same(&Renamed {
    first_name: "a",
    last_name: "b".into(),
    secret: 1,
    hidden: 2,
    r#type: 3,
    user_id_2: 4,
  });
  same(&Empty {});
  same(&Unit);
  same(&Newtype("n".into()));
  same(&Tuple(1, "t", 9, 2.5));
  same(&Transparent { inner: vec![1, 2] });
  same(&Generic { value: Some(3.25f64), list: vec!['c', '"'] });
  same(&Generic { value: Unit, list: Vec::<()>::new() });
}

#[test]
fn skipped_fields_in_every_combination() {
  for mask in 0..16u8 {
    same(&Skips {
      a: (mask & 1 != 0).then_some(1),
      b: if mask & 2 != 0 { vec![2] } else { vec![] },
      c: 3,
      d: (mask & 4 != 0).then_some("d"),
      e: "e",
    });
  }
  for mask in 0..8u8 {
    same(&AllOptional {
      a: (mask & 1 != 0).then_some(1),
      b: (mask & 2 != 0).then(|| "b".to_owned()),
      c: (mask & 4 != 0).then_some(false),
    });
  }
}

#[test]
fn enums() {
  same(&Event::Started);
  same(&Event::Stopped);
  same(&Event::Message("hi\t"));
  same(&Event::Moved(-1, 2));
  same(&Event::Nothing());
  same(&Event::LoggedIn { user_name: "u", session: None });
  same(&Event::LoggedIn { user_name: "u", session: Some(5) });
  same(&Event::LoggedOut { user_name: "u".into(), reason: 1 });
  same(&Event::Wrapped(Box::new(Event::Moved(3, 4))));
  same(&vec![Event::Started, Event::Message("m")]);
  let _: Option<Never> = None;
}

#[test]
fn std_types() {
  same(&());
  same(&(1u8, "two", 3.5f64, [4u16; 3], ('c', false)));
  same(&Some(Some(1u8)));
  same(&None::<u8>);
  same(&vec![vec![1u8], vec![]]);
  same(&VecDeque::from([1i8, -1]));
  same(&BTreeSet::from(["b", "a"]));
  same(&BTreeMap::from([("k\"1", 1u8), ("k2", 2)]));
  same(&BTreeMap::from([(1u64, "a"), (u64::MAX, "b")]));
  same(&BTreeMap::from([(-5i32, ()), (7, ())]));
  same(&BTreeMap::from([(true, 1u8), (false, 0)]));
  same(&BTreeMap::from([('x', 1u8)]));
  same(&BTreeMap::<String, u8>::new());
  same(&HashMap::from([("only".to_owned(), vec![Rc::new(1u8)])]));
  same(&Arc::new("arc"));
  same(&Cow::<str>::Borrowed("cow"));
  same(&Box::<str>::from("boxed"));
  same(&[u128::MAX, 0, 10u128.pow(20)]);
  same(&[i128::MIN, i128::MAX, -1, 0]);
  same(&[std::num::NonZeroU32::new(5).unwrap()]);
  same(&[f32::MAX, f32::MIN_POSITIVE, 0.1, -0.0, f32::NAN, f32::INFINITY, 1e-45, 16777216.0]);
}

#[test]
fn integers() {
  let mut rng = Rng(11);
  let mut values = vec![0, 1, 9, 10, 99, 100, 999, 1000, i64::MAX, i64::MIN, -1, -10, -99];
  let mut pow: i64 = 1;
  for _ in 0..18 {
    pow *= 10;
    values.extend([pow - 1, pow, pow + 1, -pow]);
  }
  for _ in 0..100_000 {
    values.push((rng.next() as i64) >> (rng.next() % 64));
  }
  same(&values);
  for value in values {
    same(&value);
    same(&(value as u64));
    same(&(value as i32));
    same(&(value as u16));
  }
}

#[test]
fn floats() {
  let mut rng = Rng(13);
  let mut values = vec![
    0.0,
    -0.0,
    0.1,
    0.5,
    1.0,
    1.5,
    100.0,
    1e21,
    1e-7,
    123_456.789,
    f64::MAX,
    f64::MIN_POSITIVE,
    5e-324,
    49.99,
    2.675,
    1e13,
    9_999_999_999_999.99,
    1e13 - 0.01,
    0.005,
    0.015,
    1.005,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
  ];
  for _ in 0..200_000 {
    values.push(f64::from_bits(rng.next()));
    values.push((rng.next() % 1_000_000_000_000_000) as f64 / 100.0);
    values.push((rng.next() % 100_000) as f64 / 1000.0);
  }
  for cents in 0..200_000u32 {
    values.push(f64::from(cents) / 100.0);
    values.push(-f64::from(cents) / 100.0);
  }
  same(&values);
  let singles: Vec<f32> = (0..200_000).map(|_| f32::from_bits(rng.next() as u32)).collect();
  same(&singles);
}

#[test]
fn wide_integers() {
  let mut rng = Rng(19);
  let mut values = vec![
    0u128,
    1,
    9,
    10,
    99,
    u64::MAX as u128,
    u64::MAX as u128 + 1,
    u128::MAX,
    u128::MAX - 1,
  ];
  let mut pow: u128 = 1;
  for _ in 0..38 {
    pow *= 10;
    values.extend([pow - 1, pow, pow + 1]);
  }
  for _ in 0..50_000 {
    let high = rng.next() as u128;
    let low = (rng.next() >> (rng.next() % 64)) as u128;
    values.push(high << 64 | low);
    values.push(low);
  }
  same(&values);
  let mut signed = vec![0i128, -1, i128::MIN, i128::MAX, i64::MIN as i128, i64::MIN as i128 - 1];
  let mut pow: i128 = 1;
  for _ in 0..38 {
    pow *= 10;
    signed.extend([pow - 1, pow, pow + 1, -pow]);
  }
  for _ in 0..50_000 {
    let high = rng.next() as i128;
    let low = (rng.next() >> (rng.next() % 64)) as i128;
    signed.push((high << 64 | low) as i128);
    signed.push(-(low as i64) as i128);
  }
  same(&signed);
}

#[test]
fn float_singles() {
  let mut rng = Rng(23);
  let mut values = vec![
    0.0f32,
    -0.0,
    0.1,
    0.5,
    1.0,
    1.5,
    49.99,
    0.005,
    1.005,
    2.675,
    99999.99,
    99999.995,
    100000.0,
    100000.01,
    123456.78,
    16777215.0,
    16777216.0,
    1e-45,
    f32::MAX,
    f32::MIN_POSITIVE,
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
  ];
  for _ in 0..200_000 {
    values.push(f32::from_bits(rng.next() as u32));
  }
  for cents in 0..200_000u32 {
    values.push(cents as f32 / 100.0);
    values.push(-(cents as f32 / 100.0));
  }
  for whole in 0..70_000u32 {
    values.push(whole as f32);
    values.push(-(whole as f32));
  }
  same(&values);
}

#[test]
fn strings() {
  let mut rng = Rng(7);
  let alphabet: Vec<char> =
    (0u8..0x80).map(char::from).chain(['é', 'ß', '東', '🎾', '\u{7f}', '/', '\u{2028}']).collect();
  for len in 0..300 {
    for _ in 0..30 {
      let density = rng.next() % 256;
      let text: String = (0..len)
        .map(|_| {
          let pick = rng.next();
          if pick % 256 >= density {
            char::from(b'a' + (pick >> 8) as u8 % 26)
          } else {
            alphabet[(pick >> 8) as usize % alphabet.len()]
          }
        })
        .collect();
      same(text.as_str());
      same(&Newtype(text));
    }
  }
}

#[test]
fn long_strings_across_escape_chunks() {
  let mut rng = Rng(17);
  let boundaries = [32 * 1024 - 1, 32 * 1024, 32 * 1024 + 1, 64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1];
  for len in [32 * 1024 - 33, 32 * 1024, 32 * 1024 + 1, 64 * 1024, 64 * 1024 + 17, 100_000] {
    // Clean except for specials placed right on the chunk boundaries.
    let mut base = vec![b'a'; len];
    for at in boundaries {
      if at < len {
        base[at] = b'"';
      }
    }
    same(String::from_utf8(base).unwrap().as_str());
    // A tenth of the bytes need escaping, all kinds.
    let dirty: String = (0..len)
      .map(|_| {
        if rng.next() % 10 == 0 {
          ['"', '\\', '\n', '\u{1}', 'é', '😀'][(rng.next() % 6) as usize]
        } else {
          'a'
        }
      })
      .collect();
    same(dirty.as_str());
  }
}

#[test]
fn to_vec_of_a_long_clean_string_does_not_reserve_six_times_the_output() {
  let text = "x".repeat(1 << 20);
  let out = rimu_json::to_vec(text.as_str());
  assert_eq!(out.len(), text.len() + 2);
  assert!(
    out.capacity() <= out.len() * 2 + 64,
    "capacity {} for {} written bytes",
    out.capacity(),
    out.len()
  );
}

#[test]
fn hand_written_with_the_writer() {
  let mut buf = Vec::new();
  {
    let mut w = JsonWriter::new(&mut buf);
    w.raw("{\"list\":[");
    for el in [1u64, 2, 3] {
      w.u64(el);
      w.raw(",");
    }
    w.end_seq(b']');
    w.raw(",\"empty\":[");
    w.end_seq(b']');
    w.raw(",\"s\":");
    w.str("q\"");
    w.raw("}");
  }
  assert_eq!(buf, br#"{"list":[1,2,3],"empty":[],"s":"q\""}"#);
}
