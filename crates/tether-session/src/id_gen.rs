use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const ADJECTIVES: &[&str] = &[
    "bold",   "brave",  "bright", "brisk",  "calm",
    "clean",  "clear",  "cool",   "crisp",  "dark",
    "deep",   "dry",    "eager",  "fair",   "fast",
    "fierce", "fine",   "firm",   "fleet",  "free",
    "fresh",  "frost",  "glad",   "gold",   "grand",
    "green",  "grey",   "happy",  "high",   "idle",
    "jade",   "keen",   "light",  "live",   "lone",
    "low",    "lush",   "mild",   "neat",   "new",
    "north",  "odd",    "pale",   "plain",  "prime",
    "proud",  "pure",   "quick",  "rare",   "red",
    "rich",   "rough",  "rust",   "safe",   "sharp",
    "shy",    "slim",   "slow",   "soft",   "south",
    "stark",  "steep",  "still",  "strong", "sure",
    "sweet",  "swift",  "tall",   "tame",   "thin",
    "true",   "vast",   "warm",   "west",   "white",
    "wide",   "wild",   "young",
];

const NOUNS: &[&str] = &[
    "ash",    "bay",    "bear",   "bee",    "birch",
    "brook",  "cave",   "cliff",  "cloud",  "colt",
    "cove",   "crane",  "creek",  "crow",   "dale",
    "dawn",   "deer",   "dew",    "dove",   "dusk",
    "elm",    "fawn",   "fern",   "finch",  "fir",
    "flame",  "flint",  "fog",    "fox",    "glen",
    "grove",  "gull",   "hare",   "hawk",   "haze",
    "heath",  "heron",  "hill",   "isle",   "ivy",
    "jay",    "lake",   "lark",   "leaf",   "lynx",
    "marsh",  "moon",   "moss",   "nest",   "oak",
    "owl",    "peak",   "pine",   "plum",   "pond",
    "quail",  "rain",   "reef",   "ridge",  "rook",
    "sage",   "shade",  "shore",  "sky",    "slope",
    "snipe",  "spring", "star",   "stone",  "stork",
    "storm",  "stream", "thorn",  "tide",   "trail",
    "trout",  "vale",   "vine",   "vole",   "wave",
    "wind",   "wren",
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
