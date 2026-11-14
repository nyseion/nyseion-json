//! `serde_json::Value`, streaming across buffer boundaries, pretty layout in chunks, I/O errors.
#![cfg(feature = "serde_json")]

use rimu_json::{JsonWrite, JsonWriter, pretty};
use serde::Serialize;
use serde_json::{Map, Number, Value, json};
use std::io;

struct Rng(u64);
impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
}

fn random_string(rng: &mut Rng) -> String {
  let alphabet = [
    'a', 'b', 'Z', ' ', '"', '\\', '\n', '\u{1}', 'é', '日', '😀', '/', '{', '}', '[', ']', ',',
    ':',
  ];
  (0..rng.next() % 40).map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize]).collect()
}

fn random_value(rng: &mut Rng, depth: u32) -> Value {
  let kind = if depth == 0 { rng.next() % 6 } else { rng.next() % 8 };
  match kind {
    0 => Value::Null,
    1 => Value::Bool(rng.next().is_multiple_of(2)),
    2 => Value::Number(Number::from(rng.next() >> (rng.next() % 64))),
    3 => Value::Number(Number::from(-((rng.next() >> 1) as i64 >> (rng.next() % 63)))),
    4 => Number::from_f64(f64::from_bits(rng.next())).map_or(Value::Null, Value::Number),
    5 => Value::String(random_string(rng)),
    6 => Value::Array((0..rng.next() % 6).map(|_| random_value(rng, depth - 1)).collect()),
    _ => Value::Object(
      (0..rng.next() % 6)
        .map(|_| (random_string(rng), random_value(rng, depth - 1)))
        .collect::<Map<_, _>>(),
    ),
  }
}

#[test]
fn values_match_serde_json() {
  let mut rng = Rng(3);
  for _ in 0..5_000 {
    let value = random_value(&mut rng, 4);
    let compact = serde_json::to_string(&value).unwrap();
    assert_eq!(rimu_json::to_string(&value), compact);
    assert_eq!(rimu_json::to_string_pretty(&value), serde_json::to_string_pretty(&value).unwrap());
    if let Value::Object(map) = &value {
      assert_eq!(rimu_json::to_string(map), compact);
    }
  }
  for value in [
    json!(0.1),
    json!(-0.0),
    json!(1e300),
    json!(u64::MAX),
    json!(i64::MIN),
    json!(49.99),
    json!({}),
    json!([]),
  ] {
    assert_eq!(rimu_json::to_string(&value), serde_json::to_string(&value).unwrap());
  }
}

/// Hand-written sequences with trailing commas and `end_seq`, long enough to stream across many
/// buffers: `end_seq` must still see the comma after a flush.
struct Commas(usize);
impl JsonWrite for Commas {
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.raw("[");
    for idx in 0..self.0 {
      w.raw("[");
      for el in 0..(idx % 7) {
        w.u64(el as u64);
        w.raw(",");
      }
      w.end_seq(b']');
      w.raw(",");
    }
    w.end_seq(b']');
  }
}

#[test]
fn streaming_matches_in_memory_across_buffers() {
  for count in [0, 1, 2, 1_000, 10_000, 100_000] {
    let value = Commas(count);
    let expected = rimu_json::to_vec(&value);
    let mut out = Vec::new();
    rimu_json::to_writer(&mut out, &value).unwrap();
    assert_eq!(out, expected, "{count}");
    let mut out = Vec::new();
    rimu_json::to_writer_pretty(&mut out, &value).unwrap();
    assert_eq!(out, rimu_json::to_vec_pretty(&value), "{count}");
  }
  let mut rng = Rng(5);
  let big: Vec<Value> = (0..3_000).map(|_| random_value(&mut rng, 4)).collect();
  let mut out = Vec::new();
  rimu_json::to_writer(&mut out, &big).unwrap();
  assert_eq!(out, serde_json::to_vec(&big).unwrap());
  let mut out = Vec::new();
  rimu_json::to_writer_pretty(&mut out, &big).unwrap();
  assert_eq!(out, serde_json::to_vec_pretty(&big).unwrap());
}

/// A writer that takes at most 3 bytes a call, and fails after `limit` bytes.
struct Trickle {
  out: Vec<u8>,
  limit: usize,
}
impl io::Write for Trickle {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.out.len() >= self.limit {
      return Err(io::Error::other("full"));
    }
    let n = buf.len().min(3);
    self.out.extend_from_slice(&buf[..n]);
    Ok(n)
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

#[test]
fn short_writes_and_errors() {
  let value = Commas(20_000);
  let mut ok = Trickle { out: Vec::new(), limit: usize::MAX };
  rimu_json::to_writer(&mut ok, &value).unwrap();
  assert_eq!(ok.out, rimu_json::to_vec(&value));
  let err = rimu_json::to_writer(Trickle { out: Vec::new(), limit: 50_000 }, &value).unwrap_err();
  assert_eq!(err.to_string(), "full");
  let err =
    rimu_json::to_writer_pretty(Trickle { out: Vec::new(), limit: 50_000 }, &value).unwrap_err();
  assert_eq!(err.to_string(), "full");
}

#[test]
fn pretty_in_chunks_of_any_size() {
  let mut rng = Rng(9);
  for _ in 0..500 {
    let value = random_value(&mut rng, 4);
    let compact = serde_json::to_vec(&value).unwrap();
    let expected = serde_json::to_vec_pretty(&value).unwrap();
    for chunk in [1, 2, 3, 7, 64] {
      let mut layout = pretty::Pretty::default();
      let mut out = Vec::new();
      for piece in compact.chunks(chunk) {
        layout.push(piece, &mut out);
      }
      layout.finish(&mut out);
      assert_eq!(out, expected, "chunk {chunk}");
    }
  }
}

#[test]
fn pretty_lays_out_any_json_text() {
  let mut rng = Rng(21);
  for _ in 0..500 {
    let value = random_value(&mut rng, 4);
    // serde_json's pretty text, itself laid out again, and with extra whitespace.
    let text = serde_json::to_string_pretty(&value).unwrap();
    let mut out = Vec::new();
    pretty::reformat(text.as_bytes(), b"  ", &mut out);
    assert_eq!(out, text.as_bytes());
    // Whitespace only outside strings: serde_json laid out with an all-whitespace indent.
    let mut loose = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" \t\r\n ");
    value.serialize(&mut serde_json::Serializer::with_formatter(&mut loose, formatter)).unwrap();
    let mut out = Vec::new();
    pretty::reformat(&loose, b"  ", &mut out);
    assert_eq!(out, text.as_bytes());
  }
}

#[test]
fn custom_indent() {
  let value = json!({"a": [1, {"b": null}], "c": {}, "d": []});
  let mut expected = Vec::new();
  let mut ser = serde_json::Serializer::with_formatter(
    &mut expected,
    serde_json::ser::PrettyFormatter::with_indent(b"\t"),
  );
  value.serialize(&mut ser).unwrap();
  let mut out = Vec::new();
  rimu_json::to_writer_with_indent(&mut out, &value, b"\t").unwrap();
  assert_eq!(out, expected);
}

#[test]
fn pretty_drops_control_bytes_outside_strings() {
  // Invalid JSON, but the input contract only promises UTF-8-preserved output. The classifier
  // treats every byte below 0x20 outside strings as whitespace, like the escape scanner does,
  // so it is dropped like `\t` between tokens would be.
  let mut out = Vec::new();
  pretty::reformat(b"\x01a\x1f\x0b", b"  ", &mut out);
  assert_eq!(out, b"a");
}

#[test]
fn pretty_survives_malformed_input() {
  let mut rng = Rng(77);
  let alphabet = b"{}[],:\"\\ a1\n\x01\x1f\x7f";
  for len in 0..200 {
    for _ in 0..50 {
      let input: Vec<u8> =
        (0..len).map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize]).collect();
      let mut out = Vec::new();
      pretty::reformat(&input, b"    ", &mut out);
    }
  }
}