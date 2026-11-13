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
mod writer;

pub use escape::{Kernel, kernel};
pub use impls::{JsonKey, JsonWrite};
#[cfg(feature = "derive")]
pub use rimu_json_derive::JsonWrite;
pub use writer::JsonWriter;

use alloc::{string::String, vec::Vec};

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
