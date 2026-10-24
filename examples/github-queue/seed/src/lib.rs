//! Synthetic scheduling policy for merge-queue acceptance, not a runtime limiter.
#![forbid(unsafe_code)]

pub fn memory_per_worker_mb() -> u64 {
    256
}

pub fn memory_budget_mb() -> u64 {
    1024
}

pub fn requested_memory_mb() -> Option<u64> {
    memory_per_worker_mb().checked_mul(worker_count())
}

pub fn worker_count() -> u64 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_least_one_worker() {
        assert!(worker_count() > 0);
    }

    #[test]
    fn minimum_worker_memory() {
        assert!(memory_per_worker_mb() >= 128);
    }

    #[test]
    fn combined_configuration_fits_budget() {
        let requested = requested_memory_mb().expect("memory arithmetic overflow");
        assert!(
            requested <= memory_budget_mb(),
            "combined configuration exceeds budget: {requested} > {}",
            memory_budget_mb()
        );
    }
}
