#[derive(Debug, Clone)]
struct Candidate<T> {
    value: T,
    weight: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct WeightedRandom<T> {
    candidates: Vec<Candidate<T>>,
}

impl<T> Default for WeightedRandom<T> {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
        }
    }
}

impl<T> WeightedRandom<T> {
    pub(crate) fn next_provider_matching(&self, predicate: impl Fn(&T) -> bool) -> Option<&T> {
        let total = self.total_weight_matching(&predicate);
        if total == 0 {
            return None;
        }
        self.select_matching(rand::random_range(0..total), predicate)
    }

    fn total_weight_matching(&self, predicate: &impl Fn(&T) -> bool) -> u128 {
        self.candidates
            .iter()
            .filter(|candidate| predicate(&candidate.value))
            .map(|candidate| candidate.weight as u128)
            .sum()
    }

    fn select_matching(&self, mut ticket: u128, predicate: impl Fn(&T) -> bool) -> Option<&T> {
        for candidate in &self.candidates {
            if !predicate(&candidate.value) {
                continue;
            }
            let weight = candidate.weight as u128;
            if ticket < weight {
                return Some(&candidate.value);
            }
            ticket -= weight;
        }
        None
    }

    pub(crate) fn find(&self, predicate: impl Fn(&T) -> bool) -> Option<&T> {
        self.candidates
            .iter()
            .find(|candidate| predicate(&candidate.value))
            .map(|candidate| &candidate.value)
    }

    pub(crate) fn upsert(
        &mut self,
        value: T,
        weight: i64,
        matches: impl Fn(&T) -> bool,
    ) -> Option<T> {
        let previous = self.remove(matches);
        if weight > 0 {
            self.candidates.push(Candidate { value, weight });
        }
        previous
    }

    pub(crate) fn remove(&mut self, predicate: impl Fn(&T) -> bool) -> Option<T> {
        let index = self
            .candidates
            .iter()
            .position(|candidate| predicate(&candidate.value))?;
        Some(self.candidates.remove(index).value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upserts_and_removes_one_candidate() {
        let mut router = WeightedRandom::default();
        router.upsert(1, 1, |_| false);
        router.upsert(2, 2, |_| false);

        assert_eq!(router.upsert(3, 4, |value| *value == 2), Some(2));
        assert_eq!(router.find(|value| *value == 1), Some(&1));
        assert_eq!(router.find(|value| *value == 3), Some(&3));
        assert_eq!(router.remove(|value| *value == 1), Some(1));
        assert_eq!(router.find(|value| *value == 1), None);
    }

    #[test]
    fn partitions_only_matching_candidates_by_weight() {
        let mut router = WeightedRandom::default();
        router.upsert(1, 100, |_| false);
        router.upsert(2, 1, |_| false);
        router.upsert(3, 2, |_| false);

        let selected = (0..3)
            .map(|ticket| *router.select_matching(ticket, |value| *value != 1).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(selected, [2, 3, 3]);
        assert_eq!(router.next_provider_matching(|value| *value == 1), Some(&1));
        assert_eq!(router.next_provider_matching(|_| false), None);
    }
}
