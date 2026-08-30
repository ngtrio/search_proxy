#[derive(Debug, Clone)]
pub struct Candidate<T> {
    pub value: T,
    pub weight: i64,
}

#[derive(Debug, Clone)]
pub struct WeightedRandom<T> {
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
    pub fn next_provider(&self) -> Option<&T> {
        let total = self.total_weight();
        if total == 0 {
            return None;
        }
        self.select(rand::random_range(0..total))
    }

    pub fn next_provider_matching(&self, predicate: impl Fn(&T) -> bool) -> Option<&T> {
        let total = self.total_weight_matching(&predicate);
        if total == 0 {
            return None;
        }
        self.select_matching(rand::random_range(0..total), predicate)
    }

    fn total_weight(&self) -> u128 {
        self.candidates
            .iter()
            .map(|candidate| candidate.weight as u128)
            .sum()
    }

    fn total_weight_matching(&self, predicate: &impl Fn(&T) -> bool) -> u128 {
        self.candidates
            .iter()
            .filter(|candidate| predicate(&candidate.value))
            .map(|candidate| candidate.weight as u128)
            .sum()
    }

    fn select(&self, mut ticket: u128) -> Option<&T> {
        for candidate in &self.candidates {
            let weight = candidate.weight as u128;
            if ticket < weight {
                return Some(&candidate.value);
            }
            ticket -= weight;
        }
        None
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

    pub fn find(&self, predicate: impl Fn(&T) -> bool) -> Option<&T> {
        self.candidates
            .iter()
            .find(|candidate| predicate(&candidate.value))
            .map(|candidate| &candidate.value)
    }

    pub fn upsert(&mut self, value: T, weight: i64, matches: impl Fn(&T) -> bool) -> Option<T> {
        let previous = self.remove(matches);
        if weight > 0 {
            self.candidates.push(Candidate { value, weight });
        }
        previous
    }

    pub fn remove(&mut self, predicate: impl Fn(&T) -> bool) -> Option<T> {
        let index = self
            .candidates
            .iter()
            .position(|candidate| predicate(&candidate.value))?;
        Some(self.candidates.remove(index).value)
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partitions_random_range_by_weight() {
        let mut router = WeightedRandom::default();
        router.upsert(1, 1, |_| false);
        router.upsert(2, 2, |_| false);
        let selected: Vec<_> = (0..3)
            .map(|ticket| *router.select(ticket).unwrap())
            .collect();
        assert_eq!(selected.iter().filter(|id| **id == 1).count(), 1);
        assert_eq!(selected.iter().filter(|id| **id == 2).count(), 2);
    }

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
