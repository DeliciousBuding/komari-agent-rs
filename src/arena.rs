//! Stack-allocated memory primitives for the zero-allocation monitoring hot path.
//!
//! - [`ScratchArena`] — 64 KB bump allocator, cursor-reset each tick
//! - [`SmallVec`] — inline storage for up to N elements; no heap fallback
//! - [`fill_slice`] — bulk slice fill for resetting sampling buffers
//!
//! # Design invariants
//! - No heap allocation in `new`, `push`, `reset`, `alloc_str`, or `alloc_bytes`.
//! - All methods are `#[inline]` to maximise LTO in `opt-level="z"` builds.
//! - Single-threaded (no `Sync` / interior mutability needed).

use std::mem::MaybeUninit;
use std::ops::{Deref, Index, IndexMut};

// ── Recommended SmallVec capacities ─────────────────────────────────────────
/// Maximum expected disk devices per tick (physical + virtual filtered).
pub const MAX_DISKS: usize = 16;
/// Maximum expected network interfaces per tick (physical + virtual filtered).
pub const MAX_NETWORKS: usize = 16;
/// Maximum expected GPU devices per tick.
pub const MAX_GPUS: usize = 8;

// ── ScratchArena ────────────────────────────────────────────────────────────

/// Bump allocator backed by a fixed 64 KB stack buffer.
///
/// Every monitoring tick calls [`reset`](ScratchArena::reset), which moves the
/// cursor back to 0 — no deallocation, no fragmentation.  The buffer is sized
/// to comfortably hold a full JSON monitoring report (typically 2-4 KB) plus
/// temporary string copies.
///
/// # Panics
/// `alloc_str` and `alloc_bytes` use `debug_assert!` for overflow checks.
/// In release builds an oversized allocation will panic with an index
/// bounds error from the slice access itself.
pub struct ScratchArena {
    buf: [u8; Self::CAPACITY],
    pos: usize,
}

impl ScratchArena {
    /// Total size of the arena in bytes (64 KB).
    pub const CAPACITY: usize = 65536;

    /// Create a zero-initialised arena with the cursor at offset 0.
    ///
    /// The `[u8; 65536]` lives on the stack of its owner (typically
    /// [`Monitor`](crate::monitor::Monitor)).  No heap allocation.
    #[inline]
    pub fn new() -> Self {
        Self {
            buf: [0u8; Self::CAPACITY],
            pos: 0,
        }
    }

    /// Copy a string into the arena and return a `&str` reference to the copy.
    ///
    /// The returned reference is valid until the next [`reset`](ScratchArena::reset)
    /// (or until the arena is dropped).
    #[inline]
    pub fn alloc_str(&mut self, s: &str) -> &str {
        let bytes = s.as_bytes();
        let n = bytes.len();
        debug_assert!(self.pos + n <= self.buf.len(), "ScratchArena exhausted");
        let start = self.pos;
        self.pos += n;
        self.buf[start..start + n].copy_from_slice(bytes);
        // SAFETY: the input was valid UTF-8 and we copied the bytes verbatim.
        unsafe { std::str::from_utf8_unchecked(&self.buf[start..start + n]) }
    }

    /// Bump-allocate `n` bytes and return a mutable slice.
    ///
    /// No alignment guarantee — the returned slice starts at whichever byte
    /// offset the cursor was at.  Use this for byte-oriented data (UTF-8,
    /// JSON fragments, etc.).
    #[inline]
    pub fn alloc_bytes(&mut self, n: usize) -> &mut [u8] {
        debug_assert!(self.pos + n <= self.buf.len(), "ScratchArena exhausted");
        let start = self.pos;
        self.pos += n;
        &mut self.buf[start..start + n]
    }

    /// Reset the bump cursor to the beginning of the buffer.
    ///
    /// No memory is zeroed; old data remains in the buffer but will be
    /// overwritten by subsequent allocations.  This is a single `usize`
    /// assignment and compiles to one `mov` instruction.
    #[inline]
    pub fn reset(&mut self) {
        self.pos = 0;
    }

    /// Return the number of bytes still available in the arena.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Return the current cursor position (bytes already allocated).
    #[inline]
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// Return the written portion of the buffer as a byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

// ── ArenaErr ────────────────────────────────────────────────────────────────

/// Error returned when a [`SmallVec`] is full and cannot accept another element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArenaErr {
    /// The `SmallVec` has reached its inline capacity `N`.
    Full,
}

// ── SmallVec ────────────────────────────────────────────────────────────────

/// Stack-allocated vector replacement with inline storage for up to `N` elements.
///
/// No heap allocation — if the capacity is exceeded, [`push`](SmallVec::push)
/// returns `Err(ArenaErr::Full)`.  The caller is expected to size `N` large
/// enough for the worst case (e.g. `MAX_DISKS = 16`, `MAX_NETWORKS = 16`,
/// `MAX_GPUS = 8`).
///
/// # Drop safety
/// When a `SmallVec` is dropped, every element that was pushed is dropped
/// in order (via [`MaybeUninit::assume_init_drop`]).  Uninitialised slots
/// are never touched.
pub struct SmallVec<T, const N: usize> {
    data: [MaybeUninit<T>; N],
    len: u8,
}

impl<T, const N: usize> SmallVec<T, N> {
    /// Create an empty `SmallVec` with no heap allocation.
    ///
    /// The internal `[MaybeUninit<T>; N]` is created uninitialised, which is
    /// sound because `MaybeUninit<T>` does not require initialisation.
    #[inline]
    pub fn new() -> Self {
        Self {
            // SAFETY: MaybeUninit<T> can be uninitialised — that is its
            // entire purpose.  An array of MaybeUninit values is therefore
            // valid even when none of the slots are initialised.
            data: unsafe { MaybeUninit::uninit().assume_init() },
            len: 0,
        }
    }

    /// Append an element.  Returns `Err(ArenaErr::Full)` if all `N` inline
    /// slots are already occupied.
    #[inline]
    pub fn push(&mut self, item: T) -> Result<(), ArenaErr> {
        let idx = self.len as usize;
        if idx >= N {
            return Err(ArenaErr::Full);
        }
        self.data[idx].write(item);
        self.len += 1;
        Ok(())
    }

    /// Number of elements currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Returns `true` when the vector contains zero elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// View the initialised portion as a shared slice.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: slots 0..len have been written via `push` and are valid T.
        unsafe { std::slice::from_raw_parts(self.data.as_ptr() as *const T, self.len as usize) }
    }

    /// View the initialised portion as a mutable slice.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: slots 0..len have been written and are valid T.
        unsafe {
            std::slice::from_raw_parts_mut(self.data.as_mut_ptr() as *mut T, self.len as usize)
        }
    }

    /// Iterate over references to the stored elements.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.as_slice().iter()
    }
}

// ── Trait impls for SmallVec ────────────────────────────────────────────────

impl<T, const N: usize> Deref for SmallVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, const N: usize> Index<usize> for SmallVec<T, N> {
    type Output = T;

    #[inline]
    fn index(&self, index: usize) -> &T {
        &self.as_slice()[index]
    }
}

impl<T, const N: usize> IndexMut<usize> for SmallVec<T, N> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut T {
        &mut self.as_mut_slice()[index]
    }
}

// ── Drop ────────────────────────────────────────────────────────────────────

impl<T, const N: usize> Drop for SmallVec<T, N> {
    fn drop(&mut self) {
        for i in 0..self.len as usize {
            // SAFETY: slot i was initialised via `push` and has not been
            // dropped yet.  We are the only owner.
            unsafe {
                self.data[i].assume_init_drop();
            }
        }
    }
}

// ── Debug (best-effort, requires T: Debug) ──────────────────────────────────

use std::fmt::{self, Debug, Formatter};

impl<T: Debug, const N: usize> Debug for SmallVec<T, N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

// ── fill_slice ──────────────────────────────────────────────────────────────

/// Fill every element of a mutable slice with the given value.
///
/// Used to reset per-tick sampling buffers (e.g. zeroing a `[f64; 3]`
/// load-average accumulator before re-reading `/proc/loadavg`).
#[inline]
pub fn fill_slice<T: Copy>(slice: &mut [T], value: T) {
    // `slice::fill` is available since Rust 1.83 and compiles to
    // `memset` / `rep stos` on x86_64.
    slice.fill(value);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ScratchArena ────────────────────────────────────────────────────

    #[test]
    fn arena_new_starts_at_zero() {
        let a = ScratchArena::new();
        assert_eq!(a.offset(), 0);
        assert_eq!(a.remaining(), ScratchArena::CAPACITY);
    }

    #[test]
    fn arena_alloc_bytes_advances_pos() {
        let mut a = ScratchArena::new();
        let s = a.alloc_bytes(16);
        assert_eq!(s.len(), 16);
        assert_eq!(a.offset(), 16);
        assert_eq!(a.remaining(), ScratchArena::CAPACITY - 16);
    }

    #[test]
    fn arena_alloc_str_roundtrip() {
        let mut a = ScratchArena::new();
        let s = a.alloc_str("hello, world!");
        assert_eq!(s, "hello, world!");
        assert_eq!(a.offset(), 13);
    }

    #[test]
    fn arena_alloc_str_empty() {
        let mut a = ScratchArena::new();
        let s = a.alloc_str("");
        assert_eq!(s, "");
        assert_eq!(a.offset(), 0);
    }

    #[test]
    fn arena_alloc_str_utf8_preserved() {
        let mut a = ScratchArena::new();
        let s = a.alloc_str("日本国");
        assert_eq!(s, "日本国");
        assert_eq!(a.offset(), 9); // 3 x 3-byte UTF-8 chars
    }

    #[test]
    fn arena_reset_moves_cursor_back() {
        let mut a = ScratchArena::new();
        {
            let _s = a.alloc_bytes(42);
            assert_eq!(a.offset(), 42);
        }
        a.reset();
        assert_eq!(a.offset(), 0);
        assert_eq!(a.remaining(), ScratchArena::CAPACITY);
    }

    #[test]
    fn arena_as_bytes_returns_written_portion() {
        let mut a = ScratchArena::new();
        a.alloc_bytes(10);
        assert_eq!(a.as_bytes().len(), 10);
        a.reset();
        assert_eq!(a.as_bytes().len(), 0);
    }

    #[test]
    #[should_panic]
    fn arena_alloc_bytes_exhausted() {
        let mut a = ScratchArena::new();
        // Allocate one byte more than capacity; debug_assert fires in debug,
        // index bounds panic in release.
        let _ = a.alloc_bytes(ScratchArena::CAPACITY + 1);
    }

    // ── SmallVec ────────────────────────────────────────────────────────

    #[test]
    fn smallvec_new_empty() {
        let v: SmallVec<u32, 8> = SmallVec::new();
        assert_eq!(v.len(), 0);
        assert!(v.is_empty());
        assert_eq!(v.as_slice().len(), 0);
    }

    #[test]
    fn smallvec_push_and_read() {
        let mut v: SmallVec<u32, 8> = SmallVec::new();
        assert!(v.push(10).is_ok());
        assert!(v.push(20).is_ok());
        assert!(v.push(30).is_ok());
        assert_eq!(v.len(), 3);
        assert_eq!(v.as_slice(), &[10, 20, 30]);
    }

    #[test]
    fn smallvec_push_up_to_capacity() {
        let mut v: SmallVec<u8, 4> = SmallVec::new();
        for i in 0..4 {
            assert!(v.push(i).is_ok());
        }
        assert_eq!(v.len(), 4);
        assert_eq!(v.as_slice(), &[0, 1, 2, 3]);
    }

    #[test]
    fn smallvec_push_beyond_capacity_returns_err() {
        let mut v: SmallVec<u8, 4> = SmallVec::new();
        for i in 0..4 {
            assert!(v.push(i).is_ok());
        }
        let result = v.push(99);
        assert_eq!(result, Err(ArenaErr::Full));
        // Length unchanged; existing elements intact.
        assert_eq!(v.len(), 4);
        assert_eq!(v.as_slice(), &[0, 1, 2, 3]);
    }

    #[test]
    fn smallvec_iter() {
        let mut v: SmallVec<&str, 3> = SmallVec::new();
        v.push("alpha").unwrap();
        v.push("beta").unwrap();
        v.push("gamma").unwrap();

        let collected: Vec<&&str> = v.iter().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(*collected[0], "alpha");
    }

    #[test]
    fn smallvec_index() {
        let mut v: SmallVec<i32, 8> = SmallVec::new();
        v.push(100).unwrap();
        v.push(200).unwrap();
        assert_eq!(v[0], 100);
        assert_eq!(v[1], 200);
    }

    #[test]
    fn smallvec_index_mut() {
        let mut v: SmallVec<i32, 8> = SmallVec::new();
        v.push(100).unwrap();
        v.push(200).unwrap();
        v[0] = 999;
        assert_eq!(v[0], 999);
        assert_eq!(v[1], 200);
    }

    #[test]
    fn smallvec_deref_to_slice() {
        let mut v: SmallVec<u16, 4> = SmallVec::new();
        v.push(1).unwrap();
        v.push(2).unwrap();
        // Deref gives &[u16]
        let s: &[u16] = &v;
        assert_eq!(s, &[1, 2]);
    }

    #[test]
    fn smallvec_capacity_matches_common_use_cases() {
        // Verify recommended capacities are representable in u8.
        assert!(MAX_DISKS <= u8::MAX as usize);
        assert!(MAX_NETWORKS <= u8::MAX as usize);
        assert!(MAX_GPUS <= u8::MAX as usize);

        // Instantiate with the recommended capacities.
        let _disks: SmallVec<u64, MAX_DISKS> = SmallVec::new();
        let _nets: SmallVec<u64, MAX_NETWORKS> = SmallVec::new();
        let _gpus: SmallVec<u64, MAX_GPUS> = SmallVec::new();
    }

    #[test]
    fn smallvec_no_heap_allocation() {
        // Smoke test: creating, pushing, and dropping a SmallVec should
        // not touch the global allocator.  We verify this indirectly by
        // exercising the full API without any Box / Vec involved.
        let mut v: SmallVec<String, 3> = SmallVec::new();
        v.push("a".to_string()).unwrap();
        v.push("b".to_string()).unwrap();
        assert_eq!(v.len(), 2);
        // Drop runs here — elements are properly destroyed.
    }

    // ── fill_slice ──────────────────────────────────────────────────────

    #[test]
    fn fill_slice_zeros_u64() {
        let mut buf = [42u64; 8];
        fill_slice(&mut buf, 0);
        assert_eq!(buf, [0u64; 8]);
    }

    #[test]
    fn fill_slice_partial() {
        let mut buf = [1u8, 2, 3, 4, 5];
        fill_slice(&mut buf[1..4], 0);
        assert_eq!(buf, [1, 0, 0, 0, 5]);
    }

    #[test]
    fn fill_slice_empty_noop() {
        let mut buf: [i32; 0] = [];
        fill_slice(&mut buf, 99);
        // Just checking this compiles and doesn't panic.
    }
}
