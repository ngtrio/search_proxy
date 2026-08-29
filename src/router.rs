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
    pub fn replace(&mut self, entries: impl IntoIterator<Item = (T, i64)>) -> Vec<T> {
        let candidates = entries
            .into_iter()
            .filter(|(_, weight)| *weight > 0)
            .map(|(value, weight)| Candidate { value, weight })
            .collect();
        std::mem::replace(&mut self.candidates, candidates)
            .into_iter()
            .map(|candidate| candidate.value)
            .collect()
    }

    pub fn next_provider(&self) -> Option<&T> {
        let total = self.total_weight();
        if total == 0 {
            return None;
        }
        self.select(rand::random_range(0..total))
    }

    fn total_weight(&self) -> u128 {
        self.candidates
            .iter()
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
        router.replace([(1, 1), (2, 2)]);
        let selected: Vec<_> = (0..3)
            .map(|ticket| *router.select(ticket).unwrap())
            .collect();
        assert_eq!(selected.iter().filter(|id| **id == 1).count(), 1);
        assert_eq!(selected.iter().filter(|id| **id == 2).count(), 2);
    }

    #[test]
    fn upserts_and_removes_one_candidate() {
        let mut router = WeightedRandom::default();
        router.replace([(1, 1), (2, 2)]);

        assert_eq!(router.upsert(3, 4, |value| *value == 2), Some(2));
        assert_eq!(router.find(|value| *value == 1), Some(&1));
        assert_eq!(router.find(|value| *value == 3), Some(&3));
        assert_eq!(router.remove(|value| *value == 1), Some(1));
        assert_eq!(router.find(|value| *value == 1), None);
    }
}
