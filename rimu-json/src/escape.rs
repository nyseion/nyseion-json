//! JSON string escaping, with the widest SIMD kernel the CPU has, chosen at run time.
//!
//! On x86-64 there are three kernels: AVX2 (32 bytes a step), SSE2 (16 bytes a step, part of the
//! x86-64 baseline, so always there) and SWAR (8 bytes a step in a `u64`, every other target).
//! A build with AVX2 enabled at compile time (`-C target-cpu=native` or `target-feature=+avx2`)
//! calls the AVX2 kernel directly. Any other x86-64 build checks the CPU once, with `cpuid` (no
//! `std` needed), caches the answer in an atomic and picks AVX2 or SSE2 from it. The check costs
//! one relaxed load per escaped string, and only strings longer than 32 bytes or with a byte to
//! escape reach here.
//!
//! Every kernel stores a whole block before it knows whether the block is clean, and resumes at
//! the first byte that needs an escape, so no byte is copied twice to its final place.

#[cfg(target_arch = "x86_64")]
use core::sync::atomic::{AtomicU8, Ordering};

/// Lowercase hex digits, as `serde_json` writes `\u00XX`.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// What a byte becomes inside a JSON string: `0` for itself, `u` for `\u00XX`, any other byte
/// `c` for `\c` (the table `serde_json` uses).
pub(crate) static ESCAPE: [u8; 256] = {
  let mut table = [0; 256];
  let mut idx = 0;
  while idx < 0x20 {
    table[idx] = b'u';
    idx += 1;
  }
  table[0x08] = b'b';
  table[0x09] = b't';
  table[0x0A] = b'n';
  table[0x0C] = b'f';
  table[0x0D] = b'r';
  table[b'"' as usize] = b'"';
  table[b'\\' as usize] = b'\\';
  table
};

pub(crate) const ONES: u64 = 0x0101_0101_0101_0101;
pub(crate) const HIGHS: u64 = 0x8080_8080_8080_8080;

/// Multiplier that gathers one flag bit from every byte of a `u64` (each byte 0 or 1) into its
/// top byte: `(flags * GATHER) >> 56` has bit `i` set when byte `i`'s flag was set. NEON has no
/// `movemask`; this and a lane extract replace it, and the SWAR classifiers use the same
/// identity to pack the per-byte hit bits of a word into a byte.
#[cfg(any(
  test,
  target_arch = "aarch64",
  not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
pub(crate) const GATHER: u64 = 0x0102_0408_1020_4080;

/// The bit `i` of every byte of `v` (which is all-zero or all-one bytes, as a compare gives),
/// packed into a `u16`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(crate) fn movemask(v: core::arch::aarch64::uint8x16_t) -> u16 {
  use core::arch::aarch64::{vreinterpretq_u64_u8, vgetq_lane_u64};
  let flags = |word: u64| {
    let flags = word & ONES;
    // Multiplication cannot overflow: the top byte is at most 0xff (bit 63) and the byte at
    // most 0x7f for the others (the largest partial product above bit 55 is bit 55 + 7 = 62).
    flags.wrapping_mul(GATHER) >> 56
  };
  // SAFETY: 0 and 1 are lanes of `v`.
  let low = unsafe { flags(u64::from(vgetq_lane_u64(vreinterpretq_u64_u8(v), 0))) };
  let high = unsafe { flags(u64::from(vgetq_lane_u64(vreinterpretq_u64_u8(v), 1))) };
  low as u16 | (high as u16) << 8
}

/// The kernels [`escape`] can run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kernel {
  /// 32 bytes a step (x86-64 with AVX2).
  Avx2,
  /// 16 bytes a step (any x86-64).
  Sse2,
  /// 16 bytes a step (any aarch64: NEON is part of the baseline).
  Neon,
  /// 8 bytes a step in a `u64` (any target).
  Swar,
}

/// The kernel [`escape`] uses on this CPU and build.
#[inline]
pub fn kernel() -> Kernel {
  #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
  {
    Kernel::Avx2
  }
  #[cfg(all(target_arch = "x86_64", not(target_feature = "avx2")))]
  {
    if x86::has_avx2() { Kernel::Avx2 } else { Kernel::Sse2 }
  }
  #[cfg(target_arch = "aarch64")]
  {
    Kernel::Neon
  }
  #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
  {
    Kernel::Swar
  }
}

/// Bytes of spare room [`escape`] needs at `dst` for `len` source bytes: every byte may become six
/// (`\u00XX`) and a block store writes up to 32 bytes ahead.
#[inline(always)]
pub(crate) fn bound(len: usize) -> usize {
  len.wrapping_mul(6).wrapping_add(32)
}

/// Copies `src` to `dst` escaped and returns how many bytes it wrote.
///
/// # Safety
///
/// `dst` is writable for [`bound`]`(src.len())` bytes.
#[inline(always)]
pub(crate) unsafe fn escape(src: &[u8], dst: *mut u8) -> usize {
  // SAFETY: forwarded; the AVX2 kernel runs only where `kernel` found AVX2.
  unsafe {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
      x86::escape_avx2(src, dst)
    }
    #[cfg(all(target_arch = "x86_64", not(target_feature = "avx2")))]
    {
      if x86::has_avx2() { x86::escape_avx2(src, dst) } else { x86::escape_sse2(src, dst) }
    }
    #[cfg(target_arch = "aarch64")]
    {
      neon::escape_neon(src, dst)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
      escape_tail(src, dst, 0, 0)
    }
  }
}

/// [`escape`] with a given kernel, for tests and benchmarks. Returns `None` when the CPU or the
/// target cannot run it.
#[doc(hidden)]
pub fn escape_with(kernel: Kernel, src: &[u8], out: &mut alloc::vec::Vec<u8>) -> Option<()> {
  out.clear();
  out.reserve(bound(src.len()));
  let dst = out.as_mut_ptr();
  // SAFETY: `bound` bytes were reserved; each kernel runs only where it is supported.
  let written = unsafe {
    match kernel {
      Kernel::Swar => escape_tail(src, dst, 0, 0),
      #[cfg(target_arch = "x86_64")]
      Kernel::Sse2 => x86::escape_sse2(src, dst),
      #[cfg(target_arch = "x86_64")]
      Kernel::Avx2 if x86::cpu_has_avx2() => x86::escape_avx2(src, dst),
      #[cfg(target_arch = "aarch64")]
      Kernel::Neon => neon::escape_neon(src, dst),
      _ => return None,
    }
  };
  // SAFETY: the kernel initialized `written` bytes.
  unsafe { out.set_len(written) };
  Some(())
}

/// The SWAR loop, then byte by byte, from `idx` in `src` and `out` in `dst`. Every kernel ends
/// here for the bytes after its last whole block.
///
/// # Safety
///
/// As [`escape`], with `idx <= src.len()` and `out` within the bound for the first `idx` bytes.
#[inline(always)]
unsafe fn escape_tail(src: &[u8], dst: *mut u8, mut idx: usize, mut out: usize) -> usize {
  let len = src.len();
  // SAFETY: loads stay inside `src`; stores within the caller's bound.
  unsafe {
    while idx.wrapping_add(8) <= len {
      let word = core::ptr::read_unaligned(src.as_ptr().add(idx).cast::<u64>());
      core::ptr::write_unaligned(dst.add(out).cast::<u64>(), word);
      let hits = word_hits(word);
      if hits == 0 {
        idx = idx.wrapping_add(8);
        out = out.wrapping_add(8);
        continue;
      }
      // A borrow can mark bytes above a true hit, never one below it, so the lowest mark is the
      // first true one.
      let clean = (hits.trailing_zeros() / 8) as usize;
      idx = idx.wrapping_add(clean);
      out = out.wrapping_add(clean);
      out = out.wrapping_add(escape_byte(*src.get_unchecked(idx), dst.add(out)));
      idx = idx.wrapping_add(1);
    }
    while idx < len {
      let byte = *src.get_unchecked(idx);
      if *ESCAPE.get_unchecked(usize::from(byte)) == 0 {
        *dst.add(out) = byte;
        out = out.wrapping_add(1);
      } else {
        out = out.wrapping_add(escape_byte(byte, dst.add(out)));
      }
      idx = idx.wrapping_add(1);
    }
  }
  out
}

/// The high bit of every byte of `word` that may need an escape: `"` and `\` XOR to zero against
/// a repeated byte; a byte below 0x20 borrows when 0x20 is subtracted.
#[inline(always)]
fn word_hits(word: u64) -> u64 {
  let zero = |el: u64| el.wrapping_sub(ONES) & !el & HIGHS;
  zero(word ^ (ONES * u64::from(b'"')))
    | zero(word ^ (ONES * u64::from(b'\\')))
    | (word.wrapping_sub(ONES * 0x20) & !word & HIGHS)
}

/// Whether a string of at most 32 bytes has a byte JSON escapes: overlapping 8-byte words for
/// 8 bytes and more, two overlapping 4-byte halves for 4 to 7, the table below that.
#[inline(always)]
pub(crate) fn has_special_short(src: &[u8]) -> bool {
  let len = src.len();
  if len < 8 {
    if len < 4 {
      return src.iter().any(|el| ESCAPE[usize::from(*el)] != 0);
    }
    let half = |at: usize| {
      src
        .get(at..at.wrapping_add(4))
        .and_then(|el| <[u8; 4]>::try_from(el).ok())
        .map_or(0, u32::from_le_bytes)
    };
    return word_hits(u64::from(half(0)) | (u64::from(half(len.wrapping_sub(4))) << 32)) != 0;
  }
  let mut idx: usize = 0;
  loop {
    let at = idx.min(len.wrapping_sub(8));
    let word = src
      .get(at..at.wrapping_add(8))
      .and_then(|el| <[u8; 8]>::try_from(el).ok())
      .map_or(0, u64::from_le_bytes);
    if word_hits(word) != 0 {
      return true;
    }
    idx = idx.wrapping_add(8);
    if idx >= len {
      return false;
    }
  }
}

/// Writes the escape of `byte`, which needs one, and returns its length.
///
/// # Safety
///
/// `dst` is writable for 6 bytes.
#[cold]
#[inline(never)]
unsafe fn escape_byte(byte: u8, dst: *mut u8) -> usize {
  let class = ESCAPE[usize::from(byte)];
  // SAFETY: at most 6 bytes are written.
  unsafe {
    *dst = b'\\';
    if class == b'u' {
      core::ptr::copy_nonoverlapping(b"u00".as_ptr(), dst.add(1), 3);
      *dst.add(4) = HEX[usize::from(byte >> 4)];
      *dst.add(5) = HEX[usize::from(byte & 0xf)];
      6
    } else {
      *dst.add(1) = class;
      2
    }
  }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
  use super::{AtomicU8, Ordering, escape_byte, escape_tail};
  use core::arch::x86_64::*;

  /// 0: not checked yet, 1: no AVX2, 2: AVX2.
  static LEVEL: AtomicU8 = AtomicU8::new(0);

  /// Whether the kernels may use AVX2: the CPU has it and, with `std`, the `RIMU_JSON_NO_AVX2`
  /// environment variable is not set (to measure the SSE2 kernel on an AVX2 machine). Checked once.
  #[inline(always)]
  #[allow(dead_code, reason = "unused when AVX2 is enabled at compile time")]
  pub(super) fn has_avx2() -> bool {
    match LEVEL.load(Ordering::Relaxed) {
      2 => true,
      1 => false,
      _ => detect(),
    }
  }

  #[cold]
  #[allow(dead_code, reason = "unused when AVX2 is enabled at compile time")]
  fn detect() -> bool {
    #[cfg(feature = "std")]
    let allowed = std::env::var_os("RIMU_JSON_NO_AVX2").is_none();
    #[cfg(not(feature = "std"))]
    let allowed = true;
    let avx2 = allowed && cpu_has_avx2();
    LEVEL.store(if avx2 { 2 } else { 1 }, Ordering::Relaxed);
    avx2
  }

  /// AVX2 support from `cpuid`, including the operating system saving the YMM registers.
  pub(super) fn cpu_has_avx2() -> bool {
    #[allow(unused_unsafe, reason = "`__cpuid` became safe in recent toolchains")]
    // SAFETY: `cpuid` is on every x86-64 CPU; `xgetbv` runs only when OSXSAVE says it can.
    unsafe {
      if __cpuid(0).eax < 7 {
        return false;
      }
      let leaf1 = __cpuid(1);
      let osxsave = leaf1.ecx & (1 << 27) != 0;
      let avx = leaf1.ecx & (1 << 28) != 0;
      if !osxsave || !avx {
        return false;
      }
      // XMM and YMM state enabled by the operating system.
      if xcr0() & 0b110 != 0b110 {
        return false;
      }
      __cpuid_count(7, 0).ebx & (1 << 5) != 0
    }
  }

  /// # Safety
  ///
  /// The CPU supports `xgetbv` (CPUID.1:ECX.OSXSAVE).
  #[target_feature(enable = "xsave")]
  unsafe fn xcr0() -> u64 {
    // SAFETY: the caller checked OSXSAVE.
    unsafe { _xgetbv(0) }
  }

  /// # Safety
  ///
  /// As [`super::escape`], on a CPU with AVX2.
  #[target_feature(enable = "avx2")]
  pub(super) unsafe fn escape_avx2(src: &[u8], dst: *mut u8) -> usize {
    let len = src.len();
    let mut idx: usize = 0;
    let mut out: usize = 0;
    let quote = _mm256_set1_epi8(b'"'.cast_signed());
    let backslash = _mm256_set1_epi8(b'\\'.cast_signed());
    let control = _mm256_set1_epi8(0x1f);
    // SAFETY: loads stay inside `src`; stores within the caller's bound.
    unsafe {
      while idx.wrapping_add(32) <= len {
        let block = _mm256_loadu_si256(src.as_ptr().add(idx).cast::<__m256i>());
        _mm256_storeu_si256(dst.add(out).cast::<__m256i>(), block);
        let hits = _mm256_or_si256(
          _mm256_or_si256(_mm256_cmpeq_epi8(block, quote), _mm256_cmpeq_epi8(block, backslash)),
          // unsigned `byte <= 0x1f`: min(byte, 0x1f) == byte
          _mm256_cmpeq_epi8(_mm256_min_epu8(block, control), block),
        );
        let mask = _mm256_movemask_epi8(hits).cast_unsigned();
        if mask == 0 {
          idx = idx.wrapping_add(32);
          out = out.wrapping_add(32);
          continue;
        }
        let clean = mask.trailing_zeros() as usize;
        idx = idx.wrapping_add(clean);
        out = out.wrapping_add(clean);
        out = out.wrapping_add(escape_byte(*src.get_unchecked(idx), dst.add(out)));
        idx = idx.wrapping_add(1);
      }
      escape_tail_sse2(src, dst, idx, out)
    }
  }

  /// # Safety
  ///
  /// As [`super::escape`].
  #[inline]
  pub(super) unsafe fn escape_sse2(src: &[u8], dst: *mut u8) -> usize {
    // SAFETY: forwarded.
    unsafe { escape_tail_sse2(src, dst, 0, 0) }
  }

  /// The SSE2 loop from `idx`/`out`, then the shared tail.
  ///
  /// # Safety
  ///
  /// As `super::escape_tail`.
  #[inline(always)]
  unsafe fn escape_tail_sse2(src: &[u8], dst: *mut u8, mut idx: usize, mut out: usize) -> usize {
    let len = src.len();
    // SAFETY: SSE2 is part of x86-64; loads stay inside `src`; stores within the caller's bound.
    unsafe {
      let quote = _mm_set1_epi8(b'"'.cast_signed());
      let backslash = _mm_set1_epi8(b'\\'.cast_signed());
      let control = _mm_set1_epi8(0x1f);
      while idx.wrapping_add(16) <= len {
        let block = _mm_loadu_si128(src.as_ptr().add(idx).cast::<__m128i>());
        _mm_storeu_si128(dst.add(out).cast::<__m128i>(), block);
        let hits = _mm_or_si128(
          _mm_or_si128(_mm_cmpeq_epi8(block, quote), _mm_cmpeq_epi8(block, backslash)),
          _mm_cmpeq_epi8(_mm_min_epu8(block, control), block),
        );
        let mask = _mm_movemask_epi8(hits).cast_unsigned();
        if mask == 0 {
          idx = idx.wrapping_add(16);
          out = out.wrapping_add(16);
          continue;
        }
        let clean = mask.trailing_zeros() as usize;
        idx = idx.wrapping_add(clean);
        out = out.wrapping_add(clean);
        out = out.wrapping_add(escape_byte(*src.get_unchecked(idx), dst.add(out)));
        idx = idx.wrapping_add(1);
      }
      escape_tail(src, dst, idx, out)
    }
  }
}

#[cfg(target_arch = "aarch64")]
mod neon {
  use super::{escape_byte, escape_tail, movemask};
  use core::arch::aarch64::*;

  /// # Safety
  ///
  /// As [`super::escape`]: NEON is part of the aarch64 baseline, so no run-time check is needed.
  #[inline]
  pub(super) unsafe fn escape_neon(src: &[u8], dst: *mut u8) -> usize {
    // SAFETY: forwarded.
    unsafe { escape_tail_neon(src, dst, 0, 0) }
  }

  /// The NEON loop from `idx`/`out`, then the shared tail.
  ///
  /// # Safety
  ///
  /// As `super::escape_tail`.
  #[inline(always)]
  unsafe fn escape_tail_neon(src: &[u8], dst: *mut u8, mut idx: usize, mut out: usize) -> usize {
    let len = src.len();
    // SAFETY: NEON is baseline on aarch64; loads stay inside `src`; stores within the caller's
    // bound.
    unsafe {
      let quote = vdupq_n_u8(b'"');
      let backslash = vdupq_n_u8(b'\\');
      let control = vdupq_n_u8(0x1f);
      while idx.wrapping_add(16) <= len {
        let block = vld1q_u8(src.as_ptr().add(idx));
        vst1q_u8(dst.add(out), block);
        let hits = vorrq_u8(
          vorrq_u8(vceqq_u8(block, quote), vceqq_u8(block, backslash)),
          // unsigned `byte <= 0x1f`: min(byte, 0x1f) == byte
          vceqq_u8(vminq_u8(block, control), block),
        );
        let mask = movemask(hits);
        if mask == 0 {
          idx = idx.wrapping_add(16);
          out = out.wrapping_add(16);
          continue;
        }
        let clean = mask.trailing_zeros() as usize;
        idx = idx.wrapping_add(clean);
        out = out.wrapping_add(clean);
        out = out.wrapping_add(escape_byte(*src.get_unchecked(idx), dst.add(out)));
        idx = idx.wrapping_add(1);
      }
      escape_tail(src, dst, idx, out)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{ESCAPE, Kernel, escape_with};
  use alloc::{format, vec::Vec};

  /// The escape of `src`, one byte at a time.
  fn reference(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &byte in src {
      match ESCAPE[usize::from(byte)] {
        0 => out.push(byte),
        b'u' => out.extend_from_slice(format!("\\u{byte:04x}").as_bytes()),
        class => out.extend_from_slice(&[b'\\', class]),
      }
    }
    out
  }

  #[test]
  fn every_kernel_matches_the_reference() {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
      state ^= state << 13;
      state ^= state >> 7;
      state ^= state << 17;
      state
    };
    let mut out = Vec::new();
    let mut ran = [0usize; 4];
    for len in 0..300 {
      for _ in 0..40 {
        let density = next() % 64;
        let src: Vec<u8> = (0..len)
          .map(|_| {
            let roll = next();
            if roll % 64 < density { (roll >> 8) as u8 } else { b'a' + (roll >> 8) as u8 % 26 }
          })
          .collect();
        let expected = reference(&src);
        for (slot, kernel) in
          [Kernel::Avx2, Kernel::Sse2, Kernel::Neon, Kernel::Swar].into_iter().enumerate()
        {
          if escape_with(kernel, &src, &mut out).is_some() {
            ran[slot] += 1;
            assert_eq!(out, expected, "{kernel:?} {src:?}");
          }
        }
      }
    }
    assert!(ran[3] > 0);
    #[cfg(target_arch = "aarch64")]
    assert!(ran[2] > 0);
  }
}
