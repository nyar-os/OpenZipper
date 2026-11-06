//! Peak-allocation evidence for the streaming archive paths.
//!
//! A counting global allocator tracks the highest live byte total, so the test
//! can state how much memory creating and extracting an archive actually used
//! instead of only asserting that it finished. The crate carries a single test
//! so no other test thread perturbs the counters.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs::{self, File},
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
};

use open_zipper_core::{CreateOptions, ExtractOptions, create, extract};

mod common;

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `action` and returns how far live bytes rose above the starting point.
fn peak_of<T>(action: impl FnOnce() -> T) -> (T, usize) {
    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let value = action();
    let peak = PEAK.load(Ordering::SeqCst).saturating_sub(baseline);
    (value, peak)
}

/// Writes `files` files of `file_bytes` zeros in fixed slices, so creating the
/// source tree itself never allocates a whole file.
fn build_source_tree(directory: &std::path::Path, files: usize, file_bytes: usize) {
    fs::create_dir_all(directory).expect("create source directory");
    let chunk = vec![0_u8; 64 * 1024];
    for index in 0..files {
        let mut file = File::create(directory.join(format!("part-{index:02}.bin"))).expect("create source file");
        let mut written = 0;
        while written < file_bytes {
            let take = chunk.len().min(file_bytes - written);
            file.write_all(&chunk[..take]).expect("write source file");
            written += take;
        }
    }
}

#[test]
fn archives_far_larger_than_memory_stream_without_expanding() {
    const FILES: usize = 8;
    const FILE_BYTES: usize = 4 * 1024 * 1024;
    const PAYLOAD: usize = FILES * FILE_BYTES;

    let base = common::TempDir::new("streaming-memory");
    let source = base.join("src");
    build_source_tree(&source, FILES, FILE_BYTES);

    let archive = base.join("large.zip");
    let (created, create_peak) = peak_of(|| create(&[source.clone()], &archive, Some("zip"), &CreateOptions::new()));
    created.expect("create zip");

    let destination = base.join("out");
    let (extracted, extract_peak) = peak_of(|| extract(&archive, &ExtractOptions::new(&destination)));
    extracted.expect("extract zip");

    println!("payload {PAYLOAD} bytes, create peak {create_peak} bytes, extract peak {extract_peak} bytes");

    // Both paths keep one 64 KiB chunk buffer plus bounded codec state live.
    // Anything close to the payload size would mean the archive was rebuilt in
    // memory, so a quarter of the payload is a generous ceiling.
    assert!(create_peak < PAYLOAD / 4, "creating a {PAYLOAD}-byte archive peaked at {create_peak} bytes");
    assert!(extract_peak < PAYLOAD / 4, "extracting a {PAYLOAD}-byte archive peaked at {extract_peak} bytes");

    for index in 0..FILES {
        let path = destination.join("src").join(format!("part-{index:02}.bin"));
        assert_eq!(fs::metadata(&path).expect("extracted file").len(), FILE_BYTES as u64);
    }
}
