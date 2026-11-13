//! `JsonWriter` over a buffer that is not a `Vec<u8>`, through `JsonBuffer`.

use rimu_json::{JsonBuffer, JsonWrite, JsonWriter};
use std::alloc::{Layout, alloc, dealloc, realloc};

/// A growable byte buffer on its own raw allocation.
struct RawBuf {
  ptr: *mut u8,
  len: usize,
  cap: usize,
}

impl RawBuf {
  fn new() -> Self {
    Self { ptr: core::ptr::NonNull::dangling().as_ptr(), len: 0, cap: 0 }
  }

  fn bytes(&self) -> &[u8] {
    // SAFETY: the first `len` bytes are initialized.
    unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
  }
}

impl Drop for RawBuf {
  fn drop(&mut self) {
    if self.cap > 0 {
      // SAFETY: allocated with this layout.
      unsafe { dealloc(self.ptr, Layout::array::<u8>(self.cap).unwrap()) };
    }
  }
}

// SAFETY: `ptr` holds `cap` bytes, the first `len` initialized; `reserve` makes room.
unsafe impl JsonBuffer for RawBuf {
  fn as_mut_ptr(&mut self) -> *mut u8 {
    self.ptr
  }
  fn len(&self) -> usize {
    self.len
  }
  fn capacity(&self) -> usize {
    self.cap
  }
  unsafe fn set_len(&mut self, len: usize) {
    self.len = len;
  }
  fn reserve(&mut self, additional: usize) {
    let need = self.len + additional;
    if need <= self.cap {
      return;
    }
    let new_cap = need.max(self.cap * 2).max(8);
    let layout = Layout::array::<u8>(new_cap).unwrap();
    // SAFETY: a fresh allocation, or a reallocation of ours with its old layout.
    self.ptr = unsafe {
      if self.cap == 0 {
        alloc(layout)
      } else {
        realloc(self.ptr, Layout::array::<u8>(self.cap).unwrap(), new_cap)
      }
    };
    assert!(!self.ptr.is_null());
    self.cap = new_cap;
  }
}

#[derive(JsonWrite)]
struct Rating {
  score: i64,
  count: u32,
}

#[derive(JsonWrite)]
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

#[test]
fn writer_over_a_custom_buffer_matches_vec() {
  let items: Vec<Item<'_>> = (0..200)
    .map(|i| Item {
      id: i,
      name: format!("Widget \"{i}\"\n"),
      category: "tools",
      price: i as f64 * 1.25,
      active: i % 2 == 0,
      tags: vec!["sale", "a\\b"],
      rating: Rating { score: -(i as i64), count: i as u32 },
      note: (i % 3 == 0).then(|| "é日😀".to_owned()),
    })
    .collect();
  let mut raw = RawBuf::new();
  {
    let mut w = JsonWriter::new(&mut raw);
    items.write_json(&mut w);
  }
  assert_eq!(raw.bytes(), rimu_json::to_vec(&items).as_slice());
}

#[test]
fn custom_buffer_grows_from_zero_capacity() {
  let mut raw = RawBuf::new();
  {
    let mut w = JsonWriter::new(&mut raw);
    for _ in 0..100_000 {
      w.str("x\"y");
    }
  }
  assert_eq!(raw.len(), 100_000 * 6);
  assert!(raw.bytes().chunks(6).all(|el| el == br#""x\"y""#));
}
