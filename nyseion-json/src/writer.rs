//! [`JsonWriter`]: JSON written straight into a `Vec<u8>`.

use crate::escape;
use alloc::vec::Vec;
use core::{marker::PhantomData, ptr};

/// `00`..`99`, two bytes a number.
const PAIRS: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Source bytes escaped per capacity check: the escaper runs on slices of this size, so a string
/// reserves room for the slice's worst case, not its own. 32 KiB keeps the overshoot of a fully
/// escaped chunk (six times 32 KiB) far under the sizes that matter while keeping the checks rare.
const ESCAPE_CHUNK: usize = 32 * 1024;

/// Appends JSON to a `Vec<u8>`.
///
/// Every method checks the spare capacity once for its own worst case and then writes without
/// further checks. Pieces that are already JSON (keys with their quotes and colon, punctuation)
/// go in with [`raw`](Self::raw); strings are escaped while they are copied; numbers are
/// formatted in place. The bytes are in the vector once the writer is dropped.
///
/// While it writes, the writer keeps the vector's pointer, length and capacity in its own fields:
/// a byte stored through the vector's own pointer could alias the vector's fields, and the
/// compiler would reload them after every piece.
#[derive(Debug)]
pub struct JsonWriter<'buf> {
  /// The buffer, type-erased: reached only through `grow_fn` and `set_len_fn`, on the cold paths.
  buf: *mut (),
  grow_fn: unsafe fn(*mut (), usize, usize) -> (*mut u8, usize),
  set_len_fn: unsafe fn(*mut (), usize),
  _buf: PhantomData<&'buf mut ()>,
  cap: usize,
  len: usize,
  ptr: *mut u8,
  /// Where full buffers go when the output streams (`to_writer`): instead of growing, the buffer
  /// is handed to the sink, all but its last byte (so that `end_seq` can still replace a
  /// trailing comma).
  sink: Option<&'buf mut dyn Sink>,
}

/// A byte buffer [`JsonWriter`] can append to: `Vec<u8>`, or any buffer of initialized bytes followed
/// by spare capacity.
///
/// # Safety
///
/// [`as_mut_ptr`](Self::as_mut_ptr) points to [`capacity`](Self::capacity) bytes of which the
/// first [`len`](Self::len) are initialized, and stays valid until the buffer is used through
/// `&mut self` again; after `reserve(n)`, `capacity() - len() >= n`; `set_len(n)` with
/// `n <= capacity()`, the first `n` bytes initialized, makes `n` the length.
pub unsafe trait JsonBuffer {
  /// The start of the buffer.
  fn as_mut_ptr(&mut self) -> *mut u8;
  /// Initialized bytes.
  fn len(&self) -> usize;
  /// Whether nothing is initialized.
  fn is_empty(&self) -> bool {
    self.len() == 0
  }
  /// Allocated bytes.
  fn capacity(&self) -> usize;
  /// Sets the length.
  ///
  /// # Safety
  ///
  /// `len <= capacity()` and the first `len` bytes are initialized.
  unsafe fn set_len(&mut self, len: usize);
  /// Makes room for at least `additional` more bytes; on allocation failure it does not return.
  fn reserve(&mut self, additional: usize);
}

// SAFETY: forwards to `Vec`, which upholds every requirement.
unsafe impl JsonBuffer for Vec<u8> {
  #[inline]
  fn as_mut_ptr(&mut self) -> *mut u8 {
    Vec::as_mut_ptr(self)
  }
  #[inline]
  fn len(&self) -> usize {
    Vec::len(self)
  }
  #[inline]
  fn capacity(&self) -> usize {
    Vec::capacity(self)
  }
  #[inline]
  unsafe fn set_len(&mut self, len: usize) {
    // SAFETY: the caller's contract is `Vec::set_len`'s.
    unsafe { Vec::set_len(self, len) }
  }
  #[inline]
  fn reserve(&mut self, additional: usize) {
    Vec::reserve(self, additional);
  }
}

/// Sets the length of `buf`, then grows it by at least `additional`; returns the new pointer and
/// capacity.
///
/// # Safety
///
/// `buf` is the `B` the writer borrows; its first `len` bytes are initialized and `len` is within
/// its capacity.
unsafe fn grow_erased<B: JsonBuffer>(
  buf: *mut (),
  len: usize,
  additional: usize,
) -> (*mut u8, usize) {
  // SAFETY: the caller's contract.
  let buf = unsafe { &mut *buf.cast::<B>() };
  // SAFETY: as above.
  unsafe { buf.set_len(len) };
  buf.reserve(additional);
  (buf.as_mut_ptr(), buf.capacity())
}

/// Sets the length of `buf`.
///
/// # Safety
///
/// As [`grow_erased`].
unsafe fn set_len_erased<B: JsonBuffer>(buf: *mut (), len: usize) {
  // SAFETY: the caller's contract.
  unsafe { (*buf.cast::<B>()).set_len(len) }
}

/// Receives the bytes of a streaming writer, buffer by buffer.
pub(crate) trait Sink {
  fn write(&mut self, bytes: &[u8]);
}

impl core::fmt::Debug for dyn Sink + '_ {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.write_str("Sink")
  }
}

// SAFETY: `&JsonWriter` gives no access to the buffer or the sink.
unsafe impl Sync for JsonWriter<'_> {}

impl<'buf> JsonWriter<'buf> {
  /// Appends to `buf`.
  #[inline]
  pub fn new<B: JsonBuffer>(buf: &'buf mut B) -> Self {
    let cap = buf.capacity();
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    Self {
      buf: core::ptr::from_mut(buf).cast(),
      grow_fn: grow_erased::<B>,
      set_len_fn: set_len_erased::<B>,
      _buf: PhantomData,
      cap,
      len,
      ptr,
      sink: None,
    }
  }

  /// A writer that hands every full buffer to `sink` instead of growing `buf`. `buf` gets at
  /// least `min_capacity` bytes; what is left in it at the end goes out with [`Self::flush_sink`].
  pub(crate) fn with_sink(
    buf: &'buf mut Vec<u8>,
    sink: &'buf mut dyn Sink,
    min_capacity: usize,
  ) -> Self {
    buf.clear();
    buf.reserve(min_capacity);
    let mut writer = Self::new(buf);
    writer.sink = Some(sink);
    writer
  }

  /// Hands everything still in the buffer to the sink.
  pub(crate) fn flush_sink(&mut self) {
    if let Some(sink) = self.sink.as_mut() {
      // SAFETY: the first `len` bytes were written.
      sink.write(unsafe { core::slice::from_raw_parts(self.ptr, self.len) });
      self.len = 0;
    }
  }

  /// Bytes in the buffer: what it held before and what was written since (when streaming, since
  /// the last buffer went out).
  #[inline]
  pub fn len(&self) -> usize {
    self.len
  }

  /// Whether nothing is in the vector.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.len == 0
  }

  /// Reserves room for at least `additional` more bytes.
  #[inline]
  pub fn reserve(&mut self, additional: usize) {
    self.ensure(additional);
  }

  /// Any [`JsonWrite`](crate::JsonWrite) value.
  #[inline(always)]
  pub fn value<T: crate::JsonWrite + ?Sized>(&mut self, value: &T) {
    value.write_json(self);
  }

  /// `true` or `false`.
  #[inline(always)]
  pub fn bool(&mut self, value: bool) {
    self.raw(if value { "true" } else { "false" });
  }

  /// `null`.
  #[inline(always)]
  pub fn null(&mut self) {
    self.raw("null");
  }

  /// Ends an array or object whose elements were each written with a trailing comma: that comma
  /// becomes `close`, and with no comma before it (an empty sequence) `close` is appended.
  #[inline(always)]
  pub fn end_seq(&mut self, close: u8) {
    assert!(close.is_ascii(), "`end_seq` takes an ASCII byte");
    if let Some(last) = self.len.checked_sub(1) {
      // SAFETY: `last < len`, a byte that was written.
      unsafe {
        let at = self.ptr.add(last);
        if *at == b',' {
          *at = close;
          return;
        }
      }
    }
    self.raw_bytes(&[close]);
  }

  /// A float in its shortest round-trip form; `null` when it is not finite, as `serde_json`.
  ///
  /// Whole values and values with at most two decimals below 10^13 (prices, ratings, quantities)
  /// take a fast path: such a value is the double nearest to `cents / 100`, a decimal of at most
  /// 15 significant digits, so that decimal is its shortest round-trip form and it is printed from
  /// the integer `cents` instead of running the general algorithm.
  #[inline]
  pub fn f64(&mut self, value: f64) {
    if !value.is_finite() {
      return self.null();
    }
    self.ensure(32);
    // SAFETY: `len <= cap`.
    let dst = unsafe { self.ptr.add(self.len) };
    let magnitude = value.abs();
    if magnitude < 1e13 {
      let int = magnitude as u64;
      if int as f64 == magnitude {
        // SAFETY: 32 bytes of spare capacity were ensured; the longest output is 16 bytes.
        unsafe {
          let neg = usize::from(value.is_sign_negative());
          *dst = b'-';
          let out = neg.wrapping_add(write_u64(int, dst.add(neg)));
          ptr::copy_nonoverlapping(b".0".as_ptr(), dst.add(out), 2);
          self.len = self.len.wrapping_add(out).wrapping_add(2);
        }
        return;
      }
      // `round` without `std`: below 10^15 < 2^52 the fraction of a double is exact.
      let scaled = magnitude * 100.0;
      let whole = scaled as u64;
      let cents = whole + u64::from(scaled - whole as f64 >= 0.5);
      if cents as f64 / 100.0 == magnitude {
        // SAFETY: 32 bytes of spare capacity were ensured; the longest output is 17 bytes.
        let written = unsafe { write_cents(value.is_sign_negative(), cents, dst) };
        self.len = self.len.wrapping_add(written);
        return;
      }
    }
    let mut buffer = zmij::Buffer::new();
    let text = buffer.format_finite(value).as_bytes();
    // SAFETY: 32 bytes of spare capacity were ensured; `zmij` writes at most 24.
    unsafe {
      copy_small(text.as_ptr(), dst, text.len());
    }
    self.len = self.len.wrapping_add(text.len());
  }

  /// A float in its shortest round-trip form as an `f32`; `null` when it is not finite.
  ///
  /// Whole values below 2^24 and values with at most two decimals below 10^5 (prices, ratings,
  /// coordinates in degrees) take a fast path, as [`f64`](Self::f64) does: such a value is the
  /// `f32` nearest to a decimal of at most six significant digits, so that decimal is its shortest
  /// round-trip form. The cents are computed in `f64`, where every `f32` is exact.
  #[inline]
  pub fn f32(&mut self, value: f32) {
    if !value.is_finite() {
      return self.null();
    }
    self.ensure(32);
    // SAFETY: `len <= cap`.
    let dst = unsafe { self.ptr.add(self.len) };
    let magnitude = value.abs();
    if magnitude < 16_777_216.0 {
      let int = magnitude as u64;
      if int as f32 == magnitude {
        // SAFETY: 32 bytes of spare capacity were ensured; the longest output is 10 bytes.
        unsafe {
          let neg = usize::from(value.is_sign_negative());
          *dst = b'-';
          let out = neg.wrapping_add(write_u64(int, dst.add(neg)));
          ptr::copy_nonoverlapping(b".0".as_ptr(), dst.add(out), 2);
          self.len = self.len.wrapping_add(out).wrapping_add(2);
        }
        return;
      }
    }
    if magnitude < 1e5 {
      // `round` without `std`: below 10^7 < 2^53 the fraction of a double is exact.
      let scaled = (magnitude as f64) * 100.0;
      let whole = scaled as u64;
      let cents = whole + u64::from(scaled - whole as f64 >= 0.5);
      if cents as f32 / 100.0 == magnitude {
        // SAFETY: 32 bytes of spare capacity were ensured; the longest output is 11 bytes.
        let written = unsafe { write_cents(value.is_sign_negative(), cents, dst) };
        self.len = self.len.wrapping_add(written);
        return;
      }
    }
    let mut buffer = zmij::Buffer::new();
    let text = buffer.format_finite(value).as_bytes();
    // SAFETY: 32 bytes of spare capacity were ensured; `zmij` writes at most 24.
    unsafe {
      copy_small(text.as_ptr(), dst, text.len());
    }
    self.len = self.len.wrapping_add(text.len());
  }

  /// A signed integer.
  #[inline(always)]
  pub fn i64(&mut self, value: i64) {
    self.ensure(20);
    // SAFETY: 20 bytes of spare capacity were ensured. The sign is stored unconditionally and the
    // first digit overwrites it when the value is not negative.
    unsafe {
      let dst = self.ptr.add(self.len);
      let neg = usize::from(value < 0);
      *dst = b'-';
      let written = write_u64(value.unsigned_abs(), dst.add(neg));
      self.len = self.len.wrapping_add(neg).wrapping_add(written);
    }
  }

  /// An unsigned integer.
  #[inline(always)]
  pub fn u64(&mut self, value: u64) {
    self.ensure(20);
    // SAFETY: 20 bytes of spare capacity were ensured and `u64` has at most 20 digits.
    unsafe {
      let written = write_u64(value, self.ptr.add(self.len));
      self.len = self.len.wrapping_add(written);
    }
  }

  /// A 128-bit signed integer.
  #[inline]
  pub fn i128(&mut self, value: i128) {
    if let Ok(small) = i64::try_from(value) {
      return self.i64(small);
    }
    self.ensure(40);
    // SAFETY: 40 bytes of spare capacity were ensured; the longest output is a sign and 39
    // digits. The sign is stored unconditionally and the first digit overwrites it when the value
    // is not negative.
    unsafe {
      let dst = self.ptr.add(self.len);
      let neg = usize::from(value < 0);
      *dst = b'-';
      let written = neg.wrapping_add(write_u128(value.unsigned_abs(), dst.add(neg)));
      self.len = self.len.wrapping_add(written);
    }
  }

  /// A 128-bit unsigned integer.
  #[inline]
  pub fn u128(&mut self, value: u128) {
    if let Ok(small) = u64::try_from(value) {
      return self.u64(small);
    }
    self.ensure(39);
    // SAFETY: 39 bytes of spare capacity were ensured; a `u128` has at most 39 digits.
    unsafe {
      let written = write_u128(value, self.ptr.add(self.len));
      self.len = self.len.wrapping_add(written);
    }
  }

  /// Text that is already JSON: keys with their quotes and colon, punctuation, literals. It is not
  /// checked.
  #[inline(always)]
  pub fn raw(&mut self, text: &str) {
    self.raw_bytes(text.as_bytes());
  }

  /// [`Self::raw`] for bytes, kept private so that the output stays UTF-8.
  #[inline(always)]
  pub(crate) fn raw_bytes(&mut self, bytes: &[u8]) {
    self.ensure(bytes.len());
    // SAFETY: `bytes.len()` bytes of spare capacity were ensured.
    unsafe {
      let dst = self.ptr.add(self.len);
      // A literal's length is a constant and the branch folds away.
      if bytes.len() <= 32 {
        copy_small(bytes.as_ptr(), dst, bytes.len());
      } else {
        ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
      }
    }
    self.len = self.len.wrapping_add(bytes.len());
  }

  /// A string, quoted and escaped.
  #[inline(always)]
  pub fn str(&mut self, value: &str) {
    let src = value.as_bytes();
    // Short strings with nothing to escape: one capacity check for the quotes and the contents.
    if src.len() <= 32 && !escape::has_special_short(src) {
      self.ensure(src.len().wrapping_add(2));
      // SAFETY: capacity for the contents and two quotes was ensured above.
      unsafe {
        let dst = self.ptr.add(self.len);
        *dst = b'"';
        copy_small(src.as_ptr(), dst.add(1), src.len());
        *dst.add(src.len().wrapping_add(1)) = b'"';
      }
      self.len = self.len.wrapping_add(src.len()).wrapping_add(2);
      return;
    }
    self.str_escaped(src, true);
  }

  /// The escaped contents of a string, without quotes.
  #[inline(always)]
  pub fn str_contents(&mut self, value: &str) {
    let src = value.as_bytes();
    // Short strings with nothing to escape, the usual case: one check, one copy.
    if src.len() <= 32 && !escape::has_special_short(src) {
      self.ensure(src.len());
      // SAFETY: capacity for `src.len()` bytes was ensured above.
      unsafe {
        copy_small(src.as_ptr(), self.ptr.add(self.len), src.len());
      }
      self.len = self.len.wrapping_add(src.len());
      return;
    }
    self.str_escaped(src, false);
  }

  #[inline(always)]
  fn ensure(&mut self, additional: usize) {
    if self.cap.wrapping_sub(self.len) < additional {
      self.grow(additional);
    }
  }

  #[cold]
  #[inline(never)]
  fn grow(&mut self, additional: usize) {
    if let Some(sink) = self.sink.as_mut()
      && self.len > 1
    {
      let keep = self.len - 1;
      // SAFETY: the first `len` bytes were written; the last one moves to the front.
      unsafe {
        sink.write(core::slice::from_raw_parts(self.ptr, keep));
        *self.ptr = *self.ptr.add(keep);
      }
      self.len = 1;
      if self.cap - self.len >= additional {
        return;
      }
    }
    // SAFETY: `buf` is the `B` the constructor erased; the first `len` bytes were written.
    let (ptr, cap) = unsafe { (self.grow_fn)(self.buf, self.len, additional.max(self.cap)) };
    self.ptr = ptr;
    self.cap = cap;
  }

  /// Escapes `src` into the buffer in chunks of [`ESCAPE_CHUNK`] source bytes, so that a clean
  /// string reserves room for what it actually writes instead of its worst case (six times its
  /// length). With `quoted`, the opening quote rides the first chunk's capacity check and the
  /// closing one the last.
  #[inline(never)]
  fn str_escaped(&mut self, src: &[u8], quoted: bool) {
    let mut at = 0;
    while at < src.len() {
      let end = (at + ESCAPE_CHUNK).min(src.len());
      let chunk = &src[at..end];
      let first = at == 0;
      let last = end == src.len();
      let quotes = usize::from(quoted) * (usize::from(first) + usize::from(last));
      self.ensure(escape::bound(chunk.len()).wrapping_add(quotes));
      // SAFETY: `escape::bound(chunk.len())` bytes of spare capacity, and `quotes` more, were
      // ensured above.
      unsafe {
        let mut dst = self.ptr.add(self.len);
        if quoted && first {
          *dst = b'"';
          dst = dst.add(1);
          self.len = self.len.wrapping_add(1);
        }
        let written = escape::escape(chunk, dst);
        self.len = self.len.wrapping_add(written);
        if quoted && last {
          // SAFETY: one more byte of spare capacity was ensured above.
          *self.ptr.add(self.len) = b'"';
          self.len = self.len.wrapping_add(1);
        }
      }
      at = end;
    }
    // An empty string never reaches the slow path, but keep the quotes paired if it does.
    if src.is_empty() && quoted {
      self.ensure(2);
      // SAFETY: two bytes of spare capacity were ensured above.
      unsafe {
        let dst = self.ptr.add(self.len);
        *dst = b'"';
        *dst.add(1) = b'"';
        self.len = self.len.wrapping_add(2);
      }
    }
  }
}

impl Drop for JsonWriter<'_> {
  #[inline]
  fn drop(&mut self) {
    // SAFETY: `buf` is the `B` the constructor erased; the first `len` bytes were written and
    // `len <= cap`.
    unsafe {
      (self.set_len_fn)(self.buf, self.len);
    }
  }
}

/// Writes `value` in decimal at `dst` and returns its length.
///
/// # Safety
///
/// `dst` is writable for 20 bytes.
#[inline(always)]
unsafe fn write_u64(value: u64, dst: *mut u8) -> usize {
  // SAFETY: at most 4 bytes are written here, at most 20 in `write_u64_long`.
  unsafe {
    if value < 100 {
      if value < 10 {
        *dst = b'0'.wrapping_add(value as u8);
        return 1;
      }
      write_pair(value as usize, dst);
      return 2;
    }
    if value < 10_000 {
      let high = value as usize / 100;
      let low = value as usize % 100;
      if high < 10 {
        *dst = b'0'.wrapping_add(high as u8);
        write_pair(low, dst.add(1));
        return 3;
      }
      write_pair(high, dst);
      write_pair(low, dst.add(2));
      return 4;
    }
    write_u64_long(value, dst)
  }
}

/// `value < 100` as two digits at `dst`.
///
/// # Safety
///
/// `dst` is writable for 2 bytes and `value < 100`.
#[inline(always)]
unsafe fn write_pair(value: usize, dst: *mut u8) {
  // SAFETY: `2 * value + 2 <= 200`, inside `PAIRS`.
  unsafe {
    ptr::copy_nonoverlapping(PAIRS.as_ptr().add(value.wrapping_mul(2)), dst, 2);
  }
}

/// [`write_u64`] for any value: two digits a step, right to left.
///
/// # Safety
///
/// `dst` is writable for 20 bytes.
#[inline]
unsafe fn write_u64_long(value: u64, dst: *mut u8) -> usize {
  let digits = value.checked_ilog10().map_or(1, |el| el.wrapping_add(1)) as usize;
  // SAFETY: `digits <= 20` bytes are written, right to left from `dst + digits`.
  unsafe {
    let mut end = dst.add(digits);
    let mut rest = value;
    while rest >= 100 {
      let pair = (rest % 100) as usize;
      rest /= 100;
      end = end.sub(2);
      write_pair(pair, end);
    }
    if rest >= 10 {
      write_pair(rest as usize, end.sub(2));
    } else {
      *end.sub(1) = b'0'.wrapping_add(rest as u8);
    }
  }
  digits
}

/// [`write_u64`] for any 128-bit value: two digits a step, right to left.
///
/// # Safety
///
/// `dst` is writable for 39 bytes.
#[inline]
unsafe fn write_u128(value: u128, dst: *mut u8) -> usize {
  let digits = value.checked_ilog10().map_or(1, |el| el.wrapping_add(1)) as usize;
  // SAFETY: `digits <= 39` bytes are written, right to left from `dst + digits`.
  unsafe {
    let mut end = dst.add(digits);
    let mut rest = value;
    while rest >= 100 {
      let pair = (rest % 100) as usize;
      rest /= 100;
      end = end.sub(2);
      write_pair(pair, end);
    }
    if rest >= 10 {
      write_pair(rest as usize, end.sub(2));
    } else {
      *end.sub(1) = b'0'.wrapping_add(rest as u8);
    }
  }
  digits
}

/// Writes `cents / 100` as `serde_json` would: `-`, the integer part, `.`, then the decimals with
/// trailing zeros dropped but at least one. Returns the length.
///
/// # Safety
///
/// `dst` is writable for 17 bytes and `cents < 10^15`.
#[inline]
unsafe fn write_cents(negative: bool, cents: u64, dst: *mut u8) -> usize {
  let frac = ((cents % 100) as usize).wrapping_mul(2);
  // SAFETY: at most 1 + 13 + 1 + 2 bytes are written.
  unsafe {
    let mut out = usize::from(negative);
    if negative {
      *dst = b'-';
    }
    out = out.wrapping_add(write_u64(cents / 100, dst.add(out)));
    *dst.add(out) = b'.';
    ptr::copy_nonoverlapping(PAIRS.as_ptr().add(frac), dst.add(out.wrapping_add(1)), 2);
    if *PAIRS.get_unchecked(frac.wrapping_add(1)) == b'0' {
      out.wrapping_add(2)
    } else {
      out.wrapping_add(3)
    }
  }
}

/// Copies `len <= 32` bytes with two overlapping moves of a fixed size instead of a `memcpy`
/// call.
///
/// # Safety
///
/// `src` is readable and `dst` writable for `len` bytes, and they do not overlap.
#[inline(always)]
pub(crate) unsafe fn copy_small(src: *const u8, dst: *mut u8, len: usize) {
  // SAFETY: each pair of moves covers `[0, len)` with reads and writes inside it.
  unsafe {
    if len >= 16 {
      ptr::copy_nonoverlapping(src, dst, 16);
      ptr::copy_nonoverlapping(src.add(len - 16), dst.add(len - 16), 16);
    } else if len >= 8 {
      ptr::copy_nonoverlapping(src, dst, 8);
      ptr::copy_nonoverlapping(src.add(len - 8), dst.add(len - 8), 8);
    } else if len >= 4 {
      ptr::copy_nonoverlapping(src, dst, 4);
      ptr::copy_nonoverlapping(src.add(len - 4), dst.add(len - 4), 4);
    } else {
      let mut idx = 0;
      while idx < len {
        *dst.add(idx) = *src.add(idx);
        idx += 1;
      }
    }
  }
}
