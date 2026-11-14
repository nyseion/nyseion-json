//! Fast JSON serialization with output identical to `serde_json`.
//!
//! - [`JsonWriter`] appends JSON to a `Vec<u8>`: pre-escaped literals, strings escaped while they
//!   are copied with the widest SIMD the CPU has (chosen at run time), integers and floats
//!   formatted in place.
//! - [`JsonWrite`] is implemented for the standard types and, with `#[derive(JsonWrite)]`, for
//!   your own. The derive writes every key with its quotes, colon and the commas around it as one
//!   literal, merged with the opening brace and with the quotes of string fields, and reads
//!   `#[serde(...)]` attributes, so a type that derives both `Serialize` and `JsonWrite` gives the
//!   same bytes from either. Attributes whose output it cannot reproduce are compile errors.
//!
//! ```
//! use rimu_json::JsonWrite;
//!
//! #[derive(JsonWrite)]
//! struct Item<'a> {
//!   id: u64,
//!   name: &'a str,
//!   #[serde(skip_serializing_if = "Option::is_none")]
//!   price: Option<f64>,
//!   tags: Vec<&'a str>,
//! }
//!
//! let item = Item { id: 7, name: "Widget \"Pro\"", price: Some(19.99), tags: vec!["sale"] };
//! assert_eq!(
//!   rimu_json::to_string(&item),
//!   r#"{"id":7,"name":"Widget \"Pro\"","price":19.99,"tags":["sale"]}"#
//! );
//! ```
#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod escape;
mod impls;
pub mod pretty;
mod writer;

pub use escape::{Kernel, kernel};
pub use impls::{JsonKey, JsonWrite};
#[cfg(feature = "derive")]
pub use rimu_json_derive::JsonWrite;
pub use writer::{JsonBuffer, JsonWriter};

use alloc::{string::String, vec::Vec};

/// Buffer size of the streaming writers: each piece handed to a sink, and each `write_all` of
/// [`to_writer`], is about this much. Large enough that a sink that is a file or socket pays one
/// syscall per chunk, small enough that the per-thread buffer stays cache-friendly.
const CHUNK: usize = 64 * 1024;

/// Appends `value` as JSON to `buf`.
#[inline]
pub fn write_to<T: JsonWrite + ?Sized>(value: &T, buf: &mut Vec<u8>) {
  value.write_json(&mut JsonWriter::new(buf));
}

/// `value` as JSON bytes.
#[inline]
pub fn to_vec<T: JsonWrite + ?Sized>(value: &T) -> Vec<u8> {
  let mut buf = Vec::with_capacity(128);
  write_to(value, &mut buf);
  buf
}

/// Appends `value` as pretty JSON to `buf`, laid out as `serde_json::to_vec_pretty` lays it out
/// (two-space indent). See [`pretty`] for other indents.
///
/// The compact form never exists in full: it is laid out chunk by chunk as it is written, so
/// peak memory is `buf` plus one chunk, not `buf` plus the whole compact document.
#[inline]
pub fn write_pretty_to<T: JsonWrite + ?Sized>(value: &T, buf: &mut Vec<u8>) {
  struct PrettyVecSink<'indent, 'buf> {
    pretty: pretty::Pretty<'indent>,
    out: &'buf mut Vec<u8>,
  }
  impl writer::Sink for PrettyVecSink<'_, '_> {
    fn write(&mut self, bytes: &[u8]) {
      self.pretty.push(bytes, self.out);
    }
  }
  with_scratch(|scratch| {
    let mut sink = PrettyVecSink { pretty: pretty::Pretty::new(b"  "), out: buf };
    {
      let mut w = JsonWriter::with_sink(scratch, &mut sink, CHUNK);
      value.write_json(&mut w);
      w.flush_sink();
    }
    sink.pretty.finish(sink.out);
  });
}

/// `value` as pretty JSON bytes, as `serde_json::to_vec_pretty`.
#[inline]
pub fn to_vec_pretty<T: JsonWrite + ?Sized>(value: &T) -> Vec<u8> {
  let mut buf = Vec::new();
  write_pretty_to(value, &mut buf);
  buf
}

/// `value` as a pretty JSON string, as `serde_json::to_string_pretty`.
#[inline]
pub fn to_string_pretty<T: JsonWrite + ?Sized>(value: &T) -> String {
  // SAFETY: as `to_string`; the layout only inserts ASCII outside strings.
  unsafe { String::from_utf8_unchecked(to_vec_pretty(value)) }
}

/// Runs `f` with an empty scratch vector: a reused per-thread one with `std` (a fresh one if
/// that one is in use, as when a `JsonWrite` impl serializes something itself), else a new one.
fn with_scratch<R>(f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
  #[cfg(feature = "std")]
  {
    std::thread_local!(static SCRATCH: core::cell::RefCell<Vec<u8>> = const { core::cell::RefCell::new(Vec::new()) });
    SCRATCH.with(|cell| match cell.try_borrow_mut() {
      Ok(mut scratch) => {
        scratch.clear();
        let out = f(&mut scratch);
        // Do not keep a huge buffer around after one huge value.
        if scratch.capacity() > 1 << 20 {
          *scratch = Vec::new();
        }
        out
      }
      Err(_) => f(&mut Vec::new()),
    })
  }
  #[cfg(not(feature = "std"))]
  {
    f(&mut Vec::new())
  }
}

#[cfg(feature = "std")]
mod io_impl {
  use crate::{CHUNK, JsonWrite, JsonWriter, pretty::Pretty, with_scratch, writer::Sink};
  use alloc::vec::Vec;
  use std::io;

  struct IoSink<W> {
    writer: W,
    error: Option<io::Error>,
  }

  impl<W: io::Write> Sink for IoSink<W> {
    fn write(&mut self, bytes: &[u8]) {
      if self.error.is_none()
        && let Err(err) = self.writer.write_all(bytes)
      {
        self.error = Some(err);
      }
    }
  }

  struct PrettySink<'indent, W> {
    pretty: Pretty<'indent>,
    out: Vec<u8>,
    io: IoSink<W>,
  }

  impl<W: io::Write> Sink for PrettySink<'_, W> {
    fn write(&mut self, bytes: &[u8]) {
      self.pretty.push(bytes, &mut self.out);
      if self.out.len() >= CHUNK {
        self.io.write(&self.out);
        self.out.clear();
      }
    }
  }

  /// Writes `value` as JSON to `writer`, streaming: the output goes out in chunks of about 64 KiB
  /// from a reused per-thread buffer, so memory does not grow with the size of the value. Like
  /// `serde_json::to_writer`, it does not flush `writer`; wrapping it in a `BufWriter` is not
  /// needed.
  pub fn to_writer<W: io::Write, T: JsonWrite + ?Sized>(writer: W, value: &T) -> io::Result<()> {
    let mut sink = IoSink { writer, error: None };
    with_scratch(|buf| {
      let mut w = JsonWriter::with_sink(buf, &mut sink, CHUNK);
      value.write_json(&mut w);
      w.flush_sink();
    });
    sink.error.map_or(Ok(()), Err)
  }

  /// [`to_writer`] with pretty output, as `serde_json::to_writer_pretty` (two-space indent).
  pub fn to_writer_pretty<W: io::Write, T: JsonWrite + ?Sized>(
    writer: W,
    value: &T,
  ) -> io::Result<()> {
    to_writer_with_indent(writer, value, b"  ")
  }

  /// [`to_writer`] with pretty output and `indent` once per level.
  pub fn to_writer_with_indent<W: io::Write, T: JsonWrite + ?Sized>(
    writer: W,
    value: &T,
    indent: &[u8],
  ) -> io::Result<()> {
    let mut sink = PrettySink {
      pretty: Pretty::new(indent),
      out: Vec::with_capacity(CHUNK * 2),
      io: IoSink { writer, error: None },
    };
    with_scratch(|buf| {
      let mut w = JsonWriter::with_sink(buf, &mut sink, CHUNK);
      value.write_json(&mut w);
      w.flush_sink();
    });
    sink.pretty.finish(&mut sink.out);
    sink.io.write(&sink.out);
    sink.io.error.map_or(Ok(()), Err)
  }
}

#[cfg(feature = "std")]
pub use io_impl::{to_writer, to_writer_pretty, to_writer_with_indent};

/// `value` as a JSON string.
#[inline]
pub fn to_string<T: JsonWrite + ?Sized>(value: &T) -> String {
  // SAFETY: the writer only ever writes UTF-8: `raw` and `str` take `&str` (escaping only
  // rewrites ASCII bytes), numbers are ASCII, and `end_seq` asserts an ASCII byte.
  unsafe { String::from_utf8_unchecked(to_vec(value)) }
}

#[doc(hidden)]
pub mod __private {
  pub use crate::escape::escape_with;

  /// Types the derive writes as string contents between quotes it merges into its literals.
  pub trait AsJsonStr {
    fn as_json_str(&self) -> &str;
  }
  impl AsJsonStr for str {
    #[inline(always)]
    fn as_json_str(&self) -> &str {
      self
    }
  }
  impl AsJsonStr for alloc::string::String {
    #[inline(always)]
    fn as_json_str(&self) -> &str {
      self
    }
  }
  impl<T: AsJsonStr + ?Sized> AsJsonStr for &T {
    #[inline(always)]
    fn as_json_str(&self) -> &str {
      (**self).as_json_str()
    }
  }
}

/// Attributes whose output the derive does not reproduce are rejected at compile time:
///
/// ```compile_fail
/// #[derive(rimu_json::JsonWrite)]
/// struct A { #[serde(flatten)] b: std::collections::BTreeMap<String, u8> }
/// ```
///
/// ```compile_fail
/// #[derive(rimu_json::JsonWrite)]
/// #[serde(tag = "type")]
/// enum E { A { x: u8 } }
/// ```
///
/// ```compile_fail
/// #[derive(rimu_json::JsonWrite)]
/// #[serde(untagged)]
/// enum E { A(u8) }
/// ```
///
/// ```compile_fail
/// #[derive(rimu_json::JsonWrite)]
/// struct A { #[serde(serialize_with = "f")] b: u8 }
/// ```
///
/// A field named `String` that is not `std`'s `String` fails to compile instead of being written
/// as a string:
///
/// ```compile_fail
/// struct String(u8);
/// impl rimu_json::JsonWrite for String {
///   fn write_json(&self, w: &mut rimu_json::JsonWriter<'_>) { w.u64(self.0.into()) }
/// }
/// #[derive(rimu_json::JsonWrite)]
/// struct A { b: String }
/// ```
#[cfg(doctest)]
pub struct RejectedAttributes;
