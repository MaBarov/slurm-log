/// Aggregate retained-script budget for one configured bank scan. The worker
/// serializes the retained bytes, so this also bounds its result file rather
/// than relying on the parent's later cache-read limit.
pub(super) struct PayloadBudget {
    remaining: u64,
}

impl PayloadBudget {
    pub(super) fn new(limit: u64) -> Self {
        Self { remaining: limit }
    }

    pub(super) fn accept(&mut self, bytes: u64) -> bool {
        let Some(remaining) = self.remaining.checked_sub(bytes) else {
            return false;
        };
        self.remaining = remaining;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_budget_is_aggregate_and_overflow_safe() {
        let mut budget = PayloadBudget::new(10);
        assert!(budget.accept(4));
        assert!(budget.accept(6));
        assert!(!budget.accept(1));
        assert!(!PayloadBudget::new(10).accept(u64::MAX));
    }
}
