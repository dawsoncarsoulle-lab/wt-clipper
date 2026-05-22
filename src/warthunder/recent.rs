use std::collections::{HashSet, VecDeque};

#[derive(Debug)]
pub struct RecentMessageCache {
    max_len: usize,
    queue: VecDeque<String>,
    set: HashSet<String>,
}

impl RecentMessageCache {
    pub fn new(max_len: usize) -> Self {
        Self {
            max_len,
            queue: VecDeque::with_capacity(max_len),
            set: HashSet::with_capacity(max_len),
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.set.contains(key)
    }

    pub fn insert(&mut self, key: String) {
        if self.max_len == 0 || self.set.contains(&key) {
            return;
        }

        self.queue.push_back(key.clone());
        self.set.insert(key);

        while self.queue.len() > self.max_len {
            if let Some(oldest) = self.queue.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.set.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_duplicate() {
        let mut cache = RecentMessageCache::new(10);

        cache.insert("chat:1".to_owned());
        cache.insert("chat:1".to_owned());

        assert!(cache.contains("chat:1"));
        assert_eq!(cache.queue.len(), 1);
        assert_eq!(cache.set.len(), 1);
    }

    #[test]
    fn evicts_old_messages_when_capacity_is_exceeded() {
        let mut cache = RecentMessageCache::new(2);

        cache.insert("chat:1".to_owned());
        cache.insert("chat:2".to_owned());
        cache.insert("chat:3".to_owned());

        assert!(!cache.contains("chat:1"));
        assert!(cache.contains("chat:2"));
        assert!(cache.contains("chat:3"));
    }
}
