//! Pretty printing, as `serde_json::to_string_pretty` formats.
//!
//! serde_json's pretty output is its compact output with tokens laid out: a newline and the indent
//! after `[` and `{` unless the container is empty, after every `,` and before every closing bracket
//! of a non-empty container, and `": "` for `:`. So pretty output is produced by writing compact
//! JSON and laying it out in one streaming pass, which needs no second serializer: [`Pretty`] takes
//! the compact bytes in chunks of any size (strings and escapes may be split anywhere) and keeps
//! only its depth and a few flags between chunks.
//!
//! The layout works on 64 bytes at a time, as simdjson's first stage does: it builds bitmasks of
//! the quotes, backslashes, structural bytes and whitespace of the block (with SSE2 on x86-64),
//! finds the escaped bytes from the runs of backslashes with one addition, and the bytes inside
//! strings as the prefix XOR of the unescaped quotes. Only the structural bytes and whitespace
//! outside strings are acted on; everything between two of them (string contents, numbers,
//! literals) is copied in one move.
//!
//! Whitespace outside strings is dropped, so [`Pretty`] also lays out any valid JSON text, not only
//! compact text. It does not validate: invalid input gives unspecified (but UTF-8 preserving)
//! output.

use crate::writer::copy_small;
use alloc::vec::Vec;
use core::ptr;

const EVEN_BITS: u64 = 0x5555_5555_5555_5555;

/// Streaming JSON layout. See the [module documentation](self).
#[derive(Clone, Debug)]
pub struct Pretty<'indent> {
  indent: &'indent [u8],
  depth: usize,
  /// All ones when the previous chunk ended inside a string.
  in_string: u64,
  /// 1 when the first byte of the next chunk is escaped (an odd run of backslashes ended the
  /// previous one).
  escaped: u64,
  /// A `[` or `{` was read and nothing after it yet: it is written once we know whether the
  /// container is empty.
  pending_open: Option<u8>,
  /// `,\n` and the indent of the deepest level so far: a newline at depth `d` is
  /// `breaks[1..2 + d * indent.len()]`, with its comma `breaks[..2 + d * indent.len()]`.
  breaks: Vec<u8>,
}

impl Default for Pretty<'static> {
  fn default() -> Self {
    Self::new(b"  ")
  }
}

/// Bitmasks of one 64-byte block, bit `i` for byte `i`.
struct Masks {
  quote: u64,
  backslash: u64,
  /// `{`, `}`, `[`, `]`, `,` and `:`.
  structural: u64,
  /// Space, `\n`, `\r` and `\t`.
  space: u64,
}

impl<'indent> Pretty<'indent> {
  /// A layout with `indent` once per level (serde_json's default is two spaces).
  pub fn new(indent: &'indent [u8]) -> Self {
    Self { indent, depth: 0, in_string: 0, escaped: 0, pending_open: None, breaks: Vec::new() }
  }

  /// Lays out `input`, the next chunk of the JSON text, appending to `out`.
  pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
    // The state and the output vector's pointer, length and capacity live in locals for the
    // loop, written through macros, so that they stay in registers.
    let indent = self.indent;
    let mut depth = self.depth;
    let mut in_string = self.in_string;
    let mut prev_escaped = self.escaped;
    let mut pending = self.pending_open;
    let mut breaks = core::mem::take(&mut self.breaks);
    out.reserve(input.len() + input.len() / 2 + 64);
    let mut ptr = out.as_mut_ptr();
    let mut olen = out.len();
    let mut cap = out.capacity();
    macro_rules! ensure {
      ($n:expr) => {{
        let n = $n;
        if cap - olen < n {
          (ptr, cap) = grow(out, olen, n);
        }
      }};
    }
    // SAFETY (both): callers ensured the room; sources are not in `out`.
    macro_rules! put {
      ($byte:expr) => {{
        unsafe { *ptr.add(olen) = $byte };
        olen += 1;
      }};
    }
    macro_rules! put_slice {
      ($slice:expr) => {{
        let slice: &[u8] = $slice;
        unsafe {
          if slice.len() <= 32 {
            copy_small(slice.as_ptr(), ptr.add(olen), slice.len());
          } else {
            ptr::copy_nonoverlapping(slice.as_ptr(), ptr.add(olen), slice.len());
          }
        }
        olen += slice.len();
      }};
    }
    // Copies `$len` bytes from `$src` in 16-byte steps, which may store and load up to 15 bytes
    // past the end: callers ensured `$len + 16` bytes of room and that `$src` is readable that
    // far.
    macro_rules! put_chunks {
      ($src:expr, $len:expr) => {{
        let (src, len): (*const u8, usize) = ($src, $len);
        let mut done = 0;
        // SAFETY: as above.
        unsafe {
          while done < len {
            ptr::copy_nonoverlapping(src.add(done), ptr.add(olen + done), 16);
            done += 16;
          }
        }
        olen += len;
      }};
    }
    // Ensures room for and writes the newline of `depth` (with its comma when `$from` is 0).
    macro_rules! newline {
      ($from:expr) => {{
        let want = 2 + depth * indent.len();
        if breaks.len() < want + 16 {
          grow_breaks(&mut breaks, indent, depth);
        }
        ensure!(want + 16);
        // `breaks` has 16 readable bytes past `want`.
        put_chunks!(breaks.as_ptr().wrapping_add($from), want - $from);
      }};
    }
    // A pending opening bracket goes out, with a newline, before anything that is not its
    // closing bracket.
    macro_rules! flush_pending {
      () => {{
        if let Some(open) = pending.take() {
          ensure!(1);
          put!(open);
          newline!(1);
        }
      }};
    }
    let len = input.len();
    let mut base = 0;
    // The block, padded with spaces: gaps are copied from here 16 bytes at a time, which may
    // read past the block's end; the padding of a partial block is masked off below.
    let mut block = [b' '; 128];
    while base < len {
      let n = (len - base).min(64);
      if n < 64 {
        block[..64].fill(b' ');
      }
      block[..n].copy_from_slice(&input[base..base + n]);
      // SAFETY: the first 64 bytes of `block`.
      let bytes: &[u8; 64] = unsafe { &*block.as_ptr().cast::<[u8; 64]>() };
      let valid = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
      let masks = classify(bytes);
      let (escaped, overflow) = find_escaped(masks.backslash & valid, prev_escaped);
      // The escape state for the next block: the carry out of a full block, or whether the byte
      // after the last one of a partial block is escaped.
      prev_escaped = if n == 64 { overflow } else { (escaped >> n) & 1 };
      let quotes = masks.quote & !escaped & valid;
      // Bits set from an opening quote up to (not including) its closing quote.
      let inside = prefix_xor(quotes) ^ in_string;
      in_string = ((inside as i64) >> 63) as u64;
      let mut acts = (masks.structural | masks.space) & !inside & valid;
      let mut pos = 0;
      while acts != 0 {
        let at = acts.trailing_zeros() as usize;
        acts &= acts - 1;
        if at > pos {
          flush_pending!();
          ensure!(at - pos + 16);
          // `pos < at < 64`: the gap and 16 bytes past it are inside `block`.
          put_chunks!(block.as_ptr().wrapping_add(pos), at - pos);
        }
        pos = at + 1;
        let byte = bytes[at];
        match byte {
          b'{' | b'[' => {
            flush_pending!();
            depth += 1;
            pending = Some(byte);
          }
          b'}' | b']' => {
            if let Some(open) = pending.take() {
              ensure!(2);
              put!(open);
              put!(byte);
              depth = depth.saturating_sub(1);
            } else {
              depth = depth.saturating_sub(1);
              newline!(1);
              ensure!(1);
              put!(byte);
            }
          }
          b',' => {
            flush_pending!();
            newline!(0);
          }
          b':' => {
            flush_pending!();
            ensure!(2);
            put_slice!(b": ");
          }
          // Whitespace.
          _ => {}
        }
      }
      if pos < n {
        flush_pending!();
        ensure!(n - pos + 16);
        // `pos < n <= 64`: as above.
        put_chunks!(block.as_ptr().wrapping_add(pos), n - pos);
      }
      base += n;
    }
    // SAFETY: the first `olen` bytes were written.
    unsafe { out.set_len(olen) };
    self.depth = depth;
    self.in_string = in_string;
    self.escaped = prev_escaped;
    self.pending_open = pending;
    self.breaks = breaks;
  }

  /// Writes what is still pending at the end of the text (an opening bracket at the very end of
  /// truncated input). Valid input leaves nothing.
  pub fn finish(&mut self, out: &mut Vec<u8>) {
    if let Some(open) = self.pending_open.take() {
      out.push(open);
    }
  }
}

/// The bytes escaped by a backslash, from the backslashes of a block and whether its first byte is
/// escaped (`prev`, 0 or 1); and the carry into the next block. A byte is escaped when an odd run
/// of backslashes ends right before it: adding the starts of the runs that begin on odd bits to
/// the backslashes carries each such run to its end, which tells runs of odd and even length
/// apart. As simdjson's `json_escape_scanner`.
#[inline(always)]
fn find_escaped(backslash: u64, prev: u64) -> (u64, u64) {
  let backslash = backslash & !prev;
  let follows_escape = (backslash << 1) | prev;
  let odd_starts = backslash & !EVEN_BITS & !follows_escape;
  let (even_runs, overflow) = odd_starts.overflowing_add(backslash);
  let invert = even_runs << 1;
  ((EVEN_BITS ^ invert) & follows_escape, u64::from(overflow))
}

/// Bit `i` is the XOR of bits `0..=i`.
#[inline(always)]
fn prefix_xor(mut bits: u64) -> u64 {
  bits ^= bits << 1;
  bits ^= bits << 2;
  bits ^= bits << 4;
  bits ^= bits << 8;
  bits ^= bits << 16;
  bits ^= bits << 32;
  bits
}

#[inline(always)]
fn classify(bytes: &[u8; 64]) -> Masks {
  #[cfg(target_arch = "x86_64")]
  // SAFETY: SSE2 is part of x86-64; the four loads are inside `bytes`.
  unsafe {
    use core::arch::x86_64::*;
    let splat = |byte: u8| _mm_set1_epi8(byte.cast_signed());
    let (quote, backslash, comma, colon) = (splat(b'"'), splat(b'\\'), splat(b','), splat(b':'));
    let (curly_open, curly_close, lower) = (splat(b'{'), splat(b'}'), splat(0x20));
    let (space, newline, carriage, tab) = (splat(b' '), splat(b'\n'), splat(b'\r'), splat(b'\t'));
    let mut masks = Masks { quote: 0, backslash: 0, structural: 0, space: 0 };
    for lane in 0..4 {
      let v = _mm_loadu_si128(bytes.as_ptr().add(lane * 16).cast());
      let bits = |m: __m128i| u64::from(_mm_movemask_epi8(m).cast_unsigned() as u16) << (lane * 16);
      masks.quote |= bits(_mm_cmpeq_epi8(v, quote));
      masks.backslash |= bits(_mm_cmpeq_epi8(v, backslash));
      // `[` and `]` are `{` and `}` without bit 0x20.
      let folded = _mm_or_si128(v, lower);
      masks.structural |= bits(_mm_or_si128(
        _mm_or_si128(_mm_cmpeq_epi8(folded, curly_open), _mm_cmpeq_epi8(folded, curly_close)),
        _mm_or_si128(_mm_cmpeq_epi8(v, comma), _mm_cmpeq_epi8(v, colon)),
      ));
      masks.space |= bits(_mm_or_si128(
        _mm_or_si128(_mm_cmpeq_epi8(v, space), _mm_cmpeq_epi8(v, newline)),
        _mm_or_si128(_mm_cmpeq_epi8(v, carriage), _mm_cmpeq_epi8(v, tab)),
      ));
    }
    masks
  }
  #[cfg(not(target_arch = "x86_64"))]
  {
    let mut masks = Masks { quote: 0, backslash: 0, structural: 0, space: 0 };
    for (idx, &byte) in bytes.iter().enumerate() {
      let bit = 1u64 << idx;
      match byte {
        b'"' => masks.quote |= bit,
        b'\\' => masks.backslash |= bit,
        b'{' | b'}' | b'[' | b']' | b',' | b':' => masks.structural |= bit,
        b' ' | b'\n' | b'\r' | b'\t' => masks.space |= bit,
        _ => {}
      }
    }
    masks
  }
}

/// Makes room for `additional` bytes after the first `len` (written) bytes of `out`: returns the
/// new pointer and capacity.
#[cold]
#[inline(never)]
fn grow(out: &mut Vec<u8>, len: usize, additional: usize) -> (*mut u8, usize) {
  // SAFETY: the first `len` bytes were written.
  unsafe { out.set_len(len) };
  out.reserve(additional.max(out.capacity()));
  (out.as_mut_ptr(), out.capacity())
}

/// Fills `breaks` with `,\n` and the indent of a few levels below `depth`.
#[cold]
fn grow_breaks(breaks: &mut Vec<u8>, indent: &[u8], depth: usize) {
  breaks.clear();
  breaks.extend_from_slice(b",\n");
  for _ in 0..(depth + 8) {
    breaks.extend_from_slice(indent);
  }
  // 16 readable bytes past the deepest newline, for the chunked copies.
  breaks.extend_from_slice(&[0; 16]);
}

/// Lays out a whole JSON text with `indent`, appending to `out`.
pub fn reformat(input: &[u8], indent: &[u8], out: &mut Vec<u8>) {
  let mut pretty = Pretty::new(indent);
  pretty.push(input, out);
  pretty.finish(out);
}
