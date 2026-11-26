use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use yield_vm::{Diagnostic, Diagnostics, Host, Platform, PlatformServices, host};

// This integration test is its own process; count only the measured thread.
thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn record_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}

// SAFETY: all allocation operations forward the original arguments to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(Some(0)));
    let result = f();
    let count = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    (result, count)
}

struct Sink;

impl Diagnostics for Sink {
    fn report(&self, _: Diagnostic<'_>) {}
}

fn empty() -> Result<Host<Platform>, String> {
    host! { services: {} }
}

fn configured() -> Result<Host<impl PlatformServices>, String> {
    host! { services: { diagnostics: Sink } }
}

#[test]
fn named_service_binding_and_access_add_no_allocations() -> Result<(), String> {
    black_box(empty()?);
    black_box(configured()?);

    let (empty, baseline) = measure(|| black_box(empty()));
    let (configured, with_service) = measure(|| black_box(configured()));
    let empty = empty?;
    let configured = configured?;
    assert_eq!(with_service, baseline);
    assert!(empty.platform().diagnostics().is_none());
    assert!(configured.platform().diagnostics().is_some());

    let (_, borrows) = measure(|| {
        for _ in 0..1024 {
            black_box(black_box(&configured).platform().diagnostics());
        }
    });
    assert_eq!(borrows, 0);
    Ok(())
}
