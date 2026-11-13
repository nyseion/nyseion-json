//! [`JsonWrite`] and [`JsonKey`] for the standard types, written as `serde_json` writes them.

use crate::JsonWriter;
use alloc::{
  borrow::{Cow, ToOwned},
  boxed::Box,
  collections::{BTreeMap, BTreeSet, LinkedList, VecDeque},
  rc::Rc,
  string::String,
  sync::Arc,
  vec::Vec,
};

/// A value that writes itself as JSON.
pub trait JsonWrite {
  /// Appends `self` to `w`.
  fn write_json(&self, w: &mut JsonWriter<'_>);
}

/// A value that can be an object key: written as a JSON string, as `serde_json` writes map keys
/// (integers and booleans quoted).
pub trait JsonKey {
  /// Appends `self` as a quoted key (without the colon) to `w`.
  fn write_key(&self, w: &mut JsonWriter<'_>);
}

macro_rules! unsigned {
  ($($ty:ty),*) => {$(
    impl JsonWrite for $ty {
      #[inline(always)]
      fn write_json(&self, w: &mut JsonWriter<'_>) { w.u64(*self as u64) }
    }
    impl JsonKey for $ty {
      #[inline(always)]
      fn write_key(&self, w: &mut JsonWriter<'_>) { w.raw("\""); w.u64(*self as u64); w.raw("\"") }
    }
  )*};
}
macro_rules! signed {
  ($($ty:ty),*) => {$(
    impl JsonWrite for $ty {
      #[inline(always)]
      fn write_json(&self, w: &mut JsonWriter<'_>) { w.i64(*self as i64) }
    }
    impl JsonKey for $ty {
      #[inline(always)]
      fn write_key(&self, w: &mut JsonWriter<'_>) { w.raw("\""); w.i64(*self as i64); w.raw("\"") }
    }
  )*};
}
unsigned!(u8, u16, u32, u64, usize);
signed!(i8, i16, i32, i64, isize);

impl JsonWrite for u128 {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.u128(*self)
  }
}
impl JsonKey for u128 {
  #[inline]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.raw("\"");
    w.u128(*self);
    w.raw("\"")
  }
}
impl JsonWrite for i128 {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.i128(*self)
  }
}
impl JsonKey for i128 {
  #[inline]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.raw("\"");
    w.i128(*self);
    w.raw("\"")
  }
}

macro_rules! nonzero {
  ($($ty:ident),*) => {$(
    impl JsonWrite for core::num::$ty {
      #[inline(always)]
      fn write_json(&self, w: &mut JsonWriter<'_>) { self.get().write_json(w) }
    }
    impl JsonKey for core::num::$ty {
      #[inline(always)]
      fn write_key(&self, w: &mut JsonWriter<'_>) { self.get().write_key(w) }
    }
  )*};
}
nonzero!(
  NonZeroU8,
  NonZeroU16,
  NonZeroU32,
  NonZeroU64,
  NonZeroU128,
  NonZeroUsize,
  NonZeroI8,
  NonZeroI16,
  NonZeroI32,
  NonZeroI64,
  NonZeroI128,
  NonZeroIsize
);

impl JsonWrite for f64 {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.f64(*self)
  }
}
impl JsonWrite for f32 {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.f32(*self)
  }
}
impl JsonWrite for bool {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.bool(*self)
  }
}
impl JsonKey for bool {
  #[inline(always)]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.raw(if *self { "\"true\"" } else { "\"false\"" })
  }
}
impl JsonWrite for () {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.null()
  }
}

impl JsonWrite for str {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.str(self)
  }
}
impl JsonKey for str {
  #[inline(always)]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.str(self)
  }
}
impl JsonWrite for String {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.str(self)
  }
}
impl JsonKey for String {
  #[inline(always)]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.str(self)
  }
}
impl JsonWrite for char {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    w.str(self.encode_utf8(&mut [0; 4]))
  }
}
impl JsonKey for char {
  #[inline(always)]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    w.str(self.encode_utf8(&mut [0; 4]))
  }
}

impl<T: JsonWrite> JsonWrite for Option<T> {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    match self {
      Some(value) => value.write_json(w),
      None => w.null(),
    }
  }
}

macro_rules! deref {
  ($($ty:ty),*) => {$(
    impl<T: JsonWrite + ?Sized> JsonWrite for $ty {
      #[inline(always)]
      fn write_json(&self, w: &mut JsonWriter<'_>) { (**self).write_json(w) }
    }
    impl<T: JsonKey + ?Sized> JsonKey for $ty {
      #[inline(always)]
      fn write_key(&self, w: &mut JsonWriter<'_>) { (**self).write_key(w) }
    }
  )*};
}
deref!(&T, &mut T, Box<T>, Rc<T>, Arc<T>);

impl<T: JsonWrite + ToOwned + ?Sized> JsonWrite for Cow<'_, T> {
  #[inline(always)]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    (**self).write_json(w)
  }
}
impl<T: JsonKey + ToOwned + ?Sized> JsonKey for Cow<'_, T> {
  #[inline(always)]
  fn write_key(&self, w: &mut JsonWriter<'_>) {
    (**self).write_key(w)
  }
}

/// `[a,b,…]` from any iterator.
#[inline]
fn seq<'a, T: JsonWrite + 'a>(w: &mut JsonWriter<'_>, mut iter: impl Iterator<Item = &'a T>) {
  w.raw("[");
  if let Some(first) = iter.next() {
    first.write_json(w);
    for el in iter {
      w.raw(",");
      el.write_json(w);
    }
  }
  w.raw("]");
}

/// `{"k":v,…}` from any iterator of pairs.
#[inline]
fn map<'a, K: JsonKey + 'a, V: JsonWrite + 'a>(
  w: &mut JsonWriter<'_>,
  mut iter: impl Iterator<Item = (&'a K, &'a V)>,
) {
  w.raw("{");
  if let Some((key, value)) = iter.next() {
    key.write_key(w);
    w.raw(":");
    value.write_json(w);
    for (key, value) in iter {
      w.raw(",");
      key.write_key(w);
      w.raw(":");
      value.write_json(w);
    }
  }
  w.raw("}");
}

impl<T: JsonWrite> JsonWrite for [T] {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<T: JsonWrite, const N: usize> JsonWrite for [T; N] {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<T: JsonWrite> JsonWrite for Vec<T> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<T: JsonWrite> JsonWrite for VecDeque<T> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<T: JsonWrite> JsonWrite for LinkedList<T> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<T: JsonWrite> JsonWrite for BTreeSet<T> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
impl<K: JsonKey, V: JsonWrite> JsonWrite for BTreeMap<K, V> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    map(w, self.iter())
  }
}
#[cfg(feature = "std")]
impl<T: JsonWrite, S> JsonWrite for std::collections::HashSet<T, S> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    seq(w, self.iter())
  }
}
#[cfg(feature = "std")]
impl<K: JsonKey, V: JsonWrite, S> JsonWrite for std::collections::HashMap<K, V, S> {
  #[inline]
  fn write_json(&self, w: &mut JsonWriter<'_>) {
    map(w, self.iter())
  }
}

macro_rules! tuple {
  ($first:ident $($name:ident)*) => {
    impl<$first: JsonWrite $(, $name: JsonWrite)*> JsonWrite for ($first, $($name,)*) {
      #[inline]
      #[allow(non_snake_case)]
      fn write_json(&self, w: &mut JsonWriter<'_>) {
        let ($first, $($name,)*) = self;
        w.raw("[");
        $first.write_json(w);
        $( w.raw(","); $name.write_json(w); )*
        w.raw("]");
      }
    }
  };
}
tuple!(A);
tuple!(A B);
tuple!(A B C);
tuple!(A B C D);
tuple!(A B C D E);
tuple!(A B C D E F);
tuple!(A B C D E F G);
tuple!(A B C D E F G H);
tuple!(A B C D E F G H I);
tuple!(A B C D E F G H I J);
tuple!(A B C D E F G H I J K);
tuple!(A B C D E F G H I J K L);

#[cfg(feature = "serde_json")]
mod serde_json_impls {
  use super::{JsonWrite, map, seq};
  use crate::JsonWriter;
  use serde_json::{Map, Number, Value};

  /// serde_json's `arbitrary_precision` feature stores a `Number` as its text (a `String`, 24
  /// bytes) instead of an enum of `u64`, `i64` and `f64` (16 bytes). Its text is then what
  /// serde_json writes, and `Display` gives it in both layouts.
  const NUMBER_IS_TEXT: bool = size_of::<Number>() != 16;

  impl JsonWrite for Number {
    #[inline]
    fn write_json(&self, w: &mut JsonWriter<'_>) {
      if !NUMBER_IS_TEXT {
        if let Some(value) = self.as_u64() {
          return w.u64(value);
        }
        if let Some(value) = self.as_i64() {
          return w.i64(value);
        }
        if let Some(value) = self.as_f64() {
          return w.f64(value);
        }
      }
      struct Raw<'a, 'b>(&'a mut JsonWriter<'b>);
      impl core::fmt::Write for Raw<'_, '_> {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
          self.0.raw(text);
          Ok(())
        }
      }
      let _ = core::fmt::write(&mut Raw(w), format_args!("{self}"));
    }
  }

  impl JsonWrite for Map<alloc::string::String, Value> {
    #[inline]
    fn write_json(&self, w: &mut JsonWriter<'_>) {
      map(w, self.iter())
    }
  }

  impl JsonWrite for Value {
    #[inline]
    fn write_json(&self, w: &mut JsonWriter<'_>) {
      match self {
        Value::Null => w.null(),
        Value::Bool(value) => w.bool(*value),
        Value::Number(value) => value.write_json(w),
        Value::String(value) => w.str(value),
        Value::Array(value) => seq(w, value.iter()),
        Value::Object(value) => map(w, value.iter()),
      }
    }
  }
}
