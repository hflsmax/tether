use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const ADJECTIVES: &[&str] = &[
    "bright", "calm", "dark", "eager", "fast",
    "glad", "happy", "idle", "keen", "light",
    "mild", "neat", "odd", "plain", "quick",
    "rare", "safe", "tall", "warm", "young",
];

const NOUNS: &[&str] = &[
    "fox", "owl", "elm", "bay", "dew",
    "sky", "oak", "bee", "fir", "jay",
    "lake", "pine", "rain", "star", "wave",
    "moon", "peak", "reef", "wind", "bear",
];

pub fn generate_id() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let adj = ADJECTIVES[n as usize % ADJECTIVES.len()];
    let noun = NOUNS[(n as usize / ADJECTIVES.len()) % NOUNS.len()];
    if n < (ADJECTIVES.len() * NOUNS.len()) as u32 {
        format!("{adj}-{noun}")
    } else {
        format!("{adj}-{noun}-{n}")
    }
}
