# rimu-json

Fast JSON serialization for Rust, with output byte-identical to `serde_json`.

Part of the rimu family of performance crates, alongside the [rimu-ws](https://github.com/vincentmele/rimu-ws)
web server. Not affiliated with the [Rimu template language](https://rimu.dev) (`rimu` on crates.io).

```rust
use rimu_json::JsonWrite;

#[derive(serde::Serialize, JsonWrite)]
#[serde(rename_all = "camelCase")]
struct Order<'a> {
  order_id: u64,
  customer_name: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  coupon: Option<String>,
  total: f64,
}

let json: String = rimu_json::to_string(&order);   // same bytes as serde_json::to_string(&order)
rimu_json::write_to(&order, &mut buf);             // appends to a reused Vec<u8>
rimu_json::to_writer(&mut file, &order)?;          // streams to any io::Write
let pretty = rimu_json::to_string_pretty(&order);  // same bytes as serde_json::to_string_pretty
rimu_json::to_writer_pretty(&mut file, &order)?;   // streaming, pretty
let value: serde_json::Value = /* ... */;
rimu_json::to_string(&value);                      // `serde_json` feature
```

## API

| | in memory | streaming to `io::Write` (`std`) |
|---|---|---|
| compact | `to_vec`, `to_string`, `write_to(&value, &mut Vec)` | `to_writer` |
| pretty (2 spaces) | `to_vec_pretty`, `to_string_pretty`, `write_pretty_to` | `to_writer_pretty` |
| pretty, any indent | `pretty::reformat`, `pretty::Pretty` | `to_writer_with_indent` |

- **Streaming** writes go out in chunks of about 64 KiB from a reused per-thread buffer, so memory
  does not grow with the value, and no `BufWriter` is needed. I/O errors are returned once the
  value is written (the rest of the output is discarded after the first one). Like serde_json,
  the writer is not flushed.
- **Pretty** output is compact output laid out by a streaming SIMD pass (`pretty::Pretty`), so it
  works for every `JsonWrite` type, including hand-written ones, with no second code path. The
  layout also accepts any valid JSON text (whitespace outside strings is dropped), so
  `pretty::reformat` doubles as a fast JSON pretty-printer.
- **`serde_json::Value`**, `Map<String, Value>` and `Number` implement `JsonWrite` with the
  `serde_json` feature, with serde_json's output also under its `arbitrary_precision` feature.

## Why it is faster

- **The derive writes literals, not calls.** Braces, keys with their quotes and colons, the
  commas between fields and the quotes around `String`/`&str` fields are merged at compile time
  into one literal per run, copied with fixed-size moves. A struct becomes alternating literal
  copies and value writes. serde goes through `serialize_struct` / `serialize_field` / key
  escaping for every field.
- **One capacity check per piece.** `JsonWriter` keeps the vector's pointer, length and capacity
  in its own fields so stores do not alias them, and checks room once per value for its worst
  case.
- **SIMD escaping while copying**, with the kernel chosen at run time: AVX2 (32 bytes a step),
  SSE2 (16, the x86-64 baseline) or SWAR (8, any target). Clean strings of up to 32 bytes skip
  the escaper entirely.
- **Pretty printing at SIMD speed.** The layout classifies 64 bytes at a time as simdjson's first
  stage does: bitmasks of quotes, backslashes, structural bytes and whitespace (SSE2), escaped
  bytes from backslash runs with one addition, string interiors as the prefix XOR of the unescaped
  quotes. Only structural bytes outside strings are acted on; everything between them is copied
  in 16-byte moves.
- **Numbers formatted in place**: integers from a two-digit table; floats through a fast path for
  whole values and values with at most two decimals (prices, ratings), otherwise `zmij`, the
  shortest round-trip algorithm `serde_json` uses.

## CPU targets

One build serves every x86-64 CPU. Without `-C target-cpu=...`, the crate reads `cpuid` once
(no `std` needed), caches the answer in an atomic, and runs the AVX2 or SSE2 kernel. With AVX2
enabled at compile time it calls the AVX2 kernel directly. `rimu_json::kernel()` reports which one
runs; `RIMU_JSON_NO_AVX2=1` forces SSE2 (with `std`) for measurement. Other targets use SWAR.

## serde compatibility

`#[derive(JsonWrite)]` reads `#[serde(...)]` (or `#[json(...)]`) attributes: `rename`,
`rename_all`, `rename_all_fields`, `skip`, `skip_serializing`, `skip_serializing_if`,
`transparent`; deserialization-only attributes are ignored. Enums are externally tagged, as serde's
default. Attributes whose output it does not reproduce (`flatten`, `tag`, `content`, `untagged`,
`serialize_with`, `with`, `into`, `remote`, `getter`, skipped variants) are **compile errors**, so
the output never silently differs from `serde_json`. The test suite checks byte equality with
`serde_json` across structs, enums, renames, every combination of skipped fields, std
collections, 128-bit integers, 600 k floats and 9 k random strings, on each escape kernel.

## Benchmarks

`cargo run --release -p bench -- <api.json> <text-file>`. Single core, Ryzen 9 5900X, default
x86-64 build (no `target-cpu`), runtime-selected AVX2. The machine was shared and loaded, so
treat ±10 % as noise. Every output was checked equal to `serde_json`'s.

| data | output | serde_json MB/s | sonic-rs MB/s | rimu derive MB/s | rimu hand MB/s | derive vs serde | derive vs sonic |
|---|---|---|---|---|---|---|---|
| API items ×10 | 1613 B | 609 | 599 | **2241** | 2404 | 3.68× | 3.74× |
| API items ×6000 | 980 KB | 463 | 606 | **1978** | 1990 | 4.28× | 3.27× |
| float points ×10 | 653 B | 504 | 498 | **775** | – | 1.54× | 1.56× |
| float points ×20000 | 1289 KB | 512 | 488 | **708** | – | 1.38× | 1.45× |
| text posts ×10 | 2513 B | 998 | 1771 | **3694** | – | 3.70× | 2.09× |
| text posts ×5000 | 1218 KB | 926 | 1477 | **2135** | – | 2.30× | 1.45× |
| orders (enum, rename, skip_if) ×10 | 2471 B | 645 | 654 | **1877** | – | 2.91× | 2.87× |
| orders (enum, rename, skip_if) ×2000 | 538 KB | 632 | 637 | **1527** | – | 2.41× | 2.40× |

### Pretty, streaming and `serde_json::Value`

Same machine and caveats. MB/s, serde_json / rimu-json (sonic-rs where it applies); pretty MB/s
are of the pretty output. For `io::Write`, serde_json goes through a `BufWriter` (as its docs
advise) and rimu-json writes directly; both into `io::sink()`.

| data | compact → Vec | compact → io::Write | pretty → Vec (serde / sonic / rimu) | pretty → io::Write |
|---|---|---|---|---|
| API items ×6000 (derive) | 552 / **1590** (2.9×) | 415 / **2332** (5.6×) | 641 / 575 / **784** (1.2×) | 530 / **711** (1.3×) |
| orders ×2000 (derive) | 622 / **1710** (2.8×) | 657 / **1541** (2.3×) | 593 / 682 / **1100** (1.9×) | 594 / **1058** (1.8×) |
| text posts ×5000 (derive) | 922 / **2503** (2.7×) | 910 / **2430** (2.7×) | 663 / 1213 / **768** (1.2×) | 618 / **742** (1.2×) |
| API items as `serde_json::Value` | 83 / **95** (1.1×) | 62 / **79** (1.3×) | 97 / – / **121** (1.2×) | 84 / **88** (1.1×) |
| orders as `serde_json::Value` | 58 / **79** (1.4×) | 68 / **80** (1.2×) | 99 / – / **116** (1.2×) | 102 / **115** (1.1×) |

`Value` gains little: walking its tree of maps dominates, and its keys are only known at run
time, so the derive's literals do not apply.

## Status and scope

Serialization only. Kernels: AVX2 (run-time detected) and SSE2 on x86_64, NEON on aarch64
(baseline, no run-time check), and a portable SWAR path everywhere else. `JsonWriter` is not
`Send` (it can carry the streaming sink of a non-`Send` writer); it is a short-lived borrow
inside `write_json`, so this rarely matters.

## Provenance and license

`JsonWriter` began in [rimu-ws](https://github.com/vincentmele/rimu-ws), where it served
HttpArena's JSON profiles; it was written there by this crate's author, not taken from wtx.

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this crate by you, as defined in the Apache-2.0 license, shall be licensed as above, without any
additional terms or conditions.
