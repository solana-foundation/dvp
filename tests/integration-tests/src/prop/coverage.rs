//! Coverage counters for property tests. A green property test only means
//! something if the interesting situations were actually generated; each
//! test prints its counters and fails if a counter is below its minimum.

use std::collections::BTreeMap;
use std::sync::Mutex;

use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

/// Test runner seeded from `PROPTEST_RNG_SEED` (u64, default 20261014), so a
/// run — and its coverage counts — is reproducible. `TestRunner::new` does
/// not read that variable in proptest 1.6.
pub fn seeded_runner() -> TestRunner {
    let seed: u64 = std::env::var("PROPTEST_RNG_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20261014);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    println!("proptest seed: {seed}");
    TestRunner::new_with_rng(
        Config {
            max_shrink_iters: 256,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &bytes),
    )
}

pub struct Coverage {
    name: &'static str,
    counts: Mutex<BTreeMap<&'static str, usize>>,
}

impl Coverage {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            counts: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn hit(&self, key: &'static str) {
        *self.counts.lock().unwrap().entry(key).or_insert(0) += 1;
    }

    /// Print all counters, then panic if any required key is under its minimum.
    pub fn report_and_check(&self, minimums: &[(&'static str, usize)]) {
        let counts = self.counts.lock().unwrap();
        println!("coverage[{}]: {:?}", self.name, *counts);
        let short: Vec<String> = minimums
            .iter()
            .filter(|(k, min)| counts.get(k).copied().unwrap_or(0) < *min)
            .map(|(k, min)| format!("{k}={} < {min}", counts.get(k).copied().unwrap_or(0)))
            .collect();
        assert!(
            short.is_empty(),
            "coverage[{}] too low: {}",
            self.name,
            short.join(", ")
        );
    }
}
