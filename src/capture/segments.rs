use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use anyhow::Context;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySegment {
    pub index: u64,
    pub path: PathBuf,
    pub modified: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StableSegmentSnapshot {
    pub found_count: usize,
    pub selected: Vec<ReplaySegment>,
    pub excluded_newest: Option<ReplaySegment>,
}

pub fn segments_to_keep(buffer_seconds: u64, segment_seconds: u64) -> usize {
    let segment_seconds = segment_seconds.max(1);
    let base = buffer_seconds.div_ceil(segment_seconds);
    (base + 2).max(2) as usize
}

pub fn segment_file_name(index: u64) -> String {
    format!("segment-{index:06}.webm")
}

pub fn segment_location_pattern(dir: &Path) -> PathBuf {
    dir.join("segment-%06d.webm")
}

pub fn list_segments(dir: &Path) -> anyhow::Result<Vec<ReplaySegment>> {
    let mut segments = Vec::new();
    if !dir.exists() {
        return Ok(segments);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(index) = parse_segment_index(&path) else {
            continue;
        };
        let metadata = entry.metadata()?;
        if !metadata.is_file() || metadata.len() == 0 {
            continue;
        }
        segments.push(ReplaySegment {
            index,
            path,
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }

    segments.sort_by_key(|segment| segment.index);
    Ok(segments)
}

pub fn prune_old_segments(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let segments = list_segments(dir)?;
    let remove_count = segments.len().saturating_sub(keep);
    for segment in segments.into_iter().take(remove_count) {
        fs::remove_file(segment.path)?;
    }
    Ok(())
}

pub fn snapshot_stable_segments(
    dir: &Path,
    keep: usize,
    segment_seconds: u64,
    now: SystemTime,
) -> anyhow::Result<StableSegmentSnapshot> {
    let mut segments = list_segments(dir)?;
    let found_count = segments.len();
    let excluded_newest = segments.pop();
    let min_age = stable_segment_min_age(segment_seconds);

    let mut selected = segments
        .into_iter()
        .filter(|segment| {
            now.duration_since(segment.modified)
                .map(|age| age >= min_age)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let start = selected.len().saturating_sub(keep);
    selected = selected.split_off(start);

    Ok(StableSegmentSnapshot {
        found_count,
        selected,
        excluded_newest,
    })
}

pub fn stable_segment_min_age(segment_seconds: u64) -> Duration {
    Duration::from_millis(1500).max(Duration::from_millis(
        segment_seconds.saturating_mul(1000) / 2,
    ))
}

pub fn selected_segments_duration(selected_count: usize, segment_seconds: u64) -> Duration {
    Duration::from_secs(selected_count as u64 * segment_seconds)
}

pub fn wait_until_segment_stable(path: &Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut previous_len = None;
    let mut stable_checks = 0;

    loop {
        let len = fs::metadata(path)
            .with_context(|| format!("failed to stat replay segment {}", path.display()))?
            .len();
        if len == 0 {
            stable_checks = 0;
            previous_len = Some(len);
        } else if previous_len == Some(len) {
            stable_checks += 1;
            if stable_checks >= 3 {
                return Ok(());
            }
        } else {
            stable_checks = 0;
            previous_len = Some(len);
        }

        if Instant::now() >= deadline {
            anyhow::bail!("segment did not become stable in time: {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn parse_segment_index(path: &Path) -> Option<u64> {
    let file_name = path.file_name()?.to_str()?;
    let index = file_name
        .strip_prefix("segment-")?
        .strip_suffix(".webm")?
        .parse()
        .ok()?;
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wt-clipper-{name}-{}", std::process::id()))
    }

    #[test]
    fn computes_segments_to_keep_with_margin() {
        assert_eq!(segments_to_keep(30, 2), 17);
        assert_eq!(segments_to_keep(10, 3), 6);
    }

    #[test]
    fn formats_segment_name() {
        assert_eq!(segment_file_name(12), "segment-000012.webm");
    }

    #[test]
    fn prunes_old_segments() {
        let dir = test_dir("prune");
        fs::create_dir_all(&dir).unwrap();
        for index in 0..4 {
            fs::write(dir.join(segment_file_name(index)), b"segment").unwrap();
        }

        prune_old_segments(&dir, 2).unwrap();
        let segments = list_segments(&dir).unwrap();

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stable_snapshot_excludes_newest_segment() {
        let dir = test_dir("stable-newest");
        fs::create_dir_all(&dir).unwrap();
        for index in 0..4 {
            fs::write(dir.join(segment_file_name(index)), b"segment").unwrap();
        }
        std::thread::sleep(Duration::from_millis(1600));

        let snapshot = snapshot_stable_segments(&dir, 10, 2, SystemTime::now()).unwrap();

        assert_eq!(snapshot.found_count, 4);
        assert_eq!(snapshot.excluded_newest.unwrap().index, 3);
        assert_eq!(
            snapshot
                .selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stable_snapshot_excludes_too_recent_segments() {
        let dir = test_dir("stable-recent");
        fs::create_dir_all(&dir).unwrap();
        for index in 0..3 {
            fs::write(dir.join(segment_file_name(index)), b"segment").unwrap();
        }

        let snapshot = snapshot_stable_segments(&dir, 10, 2, SystemTime::now()).unwrap();

        assert!(snapshot.selected.is_empty());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stable_snapshot_keeps_selected_segments_sorted() {
        let dir = test_dir("stable-sorted");
        fs::create_dir_all(&dir).unwrap();
        for index in [3, 1, 4, 0, 2] {
            fs::write(dir.join(segment_file_name(index)), b"segment").unwrap();
        }
        std::thread::sleep(Duration::from_millis(1600));

        let snapshot = snapshot_stable_segments(&dir, 3, 2, SystemTime::now()).unwrap();

        assert_eq!(
            snapshot
                .selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selected_duration_can_cover_clip_seconds() {
        assert!(selected_segments_duration(10, 2) >= Duration::from_secs(20));
    }

    #[test]
    fn wait_until_segment_stable_accepts_unchanged_file() {
        let dir = test_dir("stable-wait");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(segment_file_name(1));
        fs::write(&path, b"segment").unwrap();

        wait_until_segment_stable(&path, Duration::from_secs(1)).unwrap();

        fs::remove_dir_all(dir).unwrap();
    }
}
