use std::{
    collections::{HashSet, VecDeque},
    time::{Duration, Instant},
};

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

    pub fn len(&self) -> usize {
        self.set.len()
    }
}

#[derive(Debug)]
pub struct RecentEventCache {
    ttl: Duration,
    queue: VecDeque<(String, Instant)>,
    set: HashSet<String>,
}

impl RecentEventCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            queue: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    pub fn contains(&mut self, key: &str, now: Instant) -> bool {
        self.prune(now);
        self.set.contains(key)
    }

    pub fn insert(&mut self, key: String, now: Instant) {
        self.prune(now);
        if self.set.contains(&key) {
            return;
        }

        self.queue.push_back((key.clone(), now));
        self.set.insert(key);
    }

    pub fn insert_new(&mut self, key: String, now: Instant) -> bool {
        if self.contains(&key, now) {
            false
        } else {
            self.insert(key, now);
            true
        }
    }

    fn prune(&mut self, now: Instant) {
        while let Some((key, inserted_at)) = self.queue.front() {
            if now.duration_since(*inserted_at) <= self.ttl {
                break;
            }
            let key = key.clone();
            self.queue.pop_front();
            self.set.remove(&key);
        }
    }

    #[cfg(test)]
    pub fn len(&mut self, now: Instant) -> usize {
        self.prune(now);
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

    #[test]
    fn event_cache_ignores_duplicates_until_ttl_expires() {
        let mut cache = RecentEventCache::new(Duration::from_secs(10));
        let now = Instant::now();

        assert!(cache.insert_new("kill".to_owned(), now));
        assert!(!cache.insert_new("kill".to_owned(), now + Duration::from_secs(5)));
        assert!(cache.insert_new("kill".to_owned(), now + Duration::from_secs(11)));
        assert_eq!(cache.len(now + Duration::from_secs(11)), 1);
    }
}
