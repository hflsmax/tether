use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub fn generate_id() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    n.to_string()
}
