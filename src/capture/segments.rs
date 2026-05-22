use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySegment {
    pub index: u64,
    pub path: PathBuf,
    pub modified: SystemTime,
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

pub fn snapshot_recent_segments(dir: &Path, keep: usize) -> anyhow::Result<Vec<ReplaySegment>> {
    let mut segments = list_segments(dir)?;
    if segments.len() > 1 {
        segments.pop();
    }
    let start = segments.len().saturating_sub(keep);
    Ok(segments.split_off(start))
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
    fn snapshots_recent_finalized_segments() {
        let dir = test_dir("snapshot");
        fs::create_dir_all(&dir).unwrap();
        for index in 0..5 {
            fs::write(dir.join(segment_file_name(index)), b"segment").unwrap();
        }

        let segments = snapshot_recent_segments(&dir, 3).unwrap();

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
