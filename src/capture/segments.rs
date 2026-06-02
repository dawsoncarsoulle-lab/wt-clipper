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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSegmentSelection {
    Selected(Vec<ReplaySegment>),
    NotEnoughSegments { available: usize, needed: usize },
    NotReadyYet { reason: String },
    TooOld { reason: String },
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

    sort_segments_chronologically(&mut segments);
    Ok(segments)
}

pub fn sort_segments_chronologically(segments: &mut [ReplaySegment]) {
    segments.sort_by_key(|segment| (segment.modified, segment.index));
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

pub fn segments_needed_for_duration(duration_seconds: u64, segment_seconds: u64) -> usize {
    duration_seconds.div_ceil(segment_seconds.max(1)).max(1) as usize
}

pub fn select_segments_for_duration(
    segments: &[ReplaySegment],
    duration_seconds: u64,
    segment_seconds: u64,
) -> Vec<ReplaySegment> {
    let mut segments = segments.to_vec();
    sort_segments_chronologically(&mut segments);
    let needed = segments_needed_for_duration(duration_seconds, segment_seconds);
    let start = segments.len().saturating_sub(needed);
    segments[start..].to_vec()
}

pub fn select_segments_around_event(
    segments: &[ReplaySegment],
    first_event_time: SystemTime,
    last_event_time: SystemTime,
    duration_seconds: u64,
    post_event_seconds: u64,
    segment_seconds: u64,
) -> EventSegmentSelection {
    let mut segments = segments.to_vec();
    sort_segments_chronologically(&mut segments);
    let target_end = last_event_time + Duration::from_secs(post_event_seconds);
    let expected_start = first_event_time
        .checked_sub(Duration::from_secs(
            duration_seconds.saturating_sub(post_event_seconds),
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let needed = target_end
        .duration_since(expected_start)
        .map(|duration| duration.as_secs() + u64::from(duration.subsec_nanos() > 0))
        .map(|seconds| segments_needed_for_duration(seconds, segment_seconds))
        .unwrap_or_else(|_| segments_needed_for_duration(duration_seconds, segment_seconds));

    debug_segment_selection_inputs(
        &segments,
        first_event_time,
        last_event_time,
        target_end,
        expected_start,
        needed,
    );

    if segments.len() < needed {
        return EventSegmentSelection::NotEnoughSegments {
            available: segments.len(),
            needed,
        };
    }

    let Some(first_segment) = segments.first() else {
        return EventSegmentSelection::NotEnoughSegments {
            available: 0,
            needed,
        };
    };
    let Some(last_segment) = segments.last() else {
        return EventSegmentSelection::NotEnoughSegments {
            available: 0,
            needed,
        };
    };

    let earliest_start = first_segment
        .modified
        .checked_sub(Duration::from_secs(segment_seconds))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if earliest_start > first_event_time
        || earliest_start > expected_start + Duration::from_secs(segment_seconds)
    {
        return EventSegmentSelection::TooOld {
            reason: format!(
                "required start is older than stable replay window: expected_start={expected_start:?}, earliest_start={earliest_start:?}"
            ),
        };
    }

    if last_segment.modified < target_end {
        return EventSegmentSelection::NotReadyYet {
            reason: format!(
                "stable segments do not cover post-event window yet: target_end={target_end:?}, latest_stable_end={:?}",
                last_segment.modified
            ),
        };
    }

    let mut end_index = segments
        .iter()
        .position(|segment| segment.modified >= target_end)
        .unwrap_or(segments.len().saturating_sub(1));
    if end_index + 1 < needed {
        end_index = needed - 1;
    }

    let start_index = end_index + 1 - needed;
    let selected = segments[start_index..=end_index].to_vec();
    let selected_start = selected
        .first()
        .and_then(|segment| {
            segment
                .modified
                .checked_sub(Duration::from_secs(segment_seconds))
        })
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let selected_end = selected
        .last()
        .map(|segment| segment.modified)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if selected_start > first_event_time
        || selected_end < last_event_time
        || selected_end < target_end
    {
        return EventSegmentSelection::NotReadyYet {
            reason: format!(
                "selected stable segments do not cover event window yet: selected_start={selected_start:?}, selected_end={selected_end:?}"
            ),
        };
    }
    if selected_start > expected_start + Duration::from_secs(segment_seconds) {
        return EventSegmentSelection::TooOld {
            reason: format!(
                "selected replay window starts too late: expected_start={expected_start:?}, selected_start={selected_start:?}"
            ),
        };
    }

    println!(
        "[CLIP] selected chronological indexes: {:?}",
        selected
            .iter()
            .map(|segment| segment.index)
            .collect::<Vec<_>>()
    );
    println!(
        "[CLIP] expected_start={expected_start:?} target_end={target_end:?} earliest_selected_start={selected_start:?} latest_selected_end={selected_end:?}"
    );
    EventSegmentSelection::Selected(selected)
}

fn debug_segment_selection_inputs(
    segments: &[ReplaySegment],
    first_event_time: SystemTime,
    last_event_time: SystemTime,
    target_end: SystemTime,
    expected_start: SystemTime,
    needed: usize,
) {
    println!("[CLIP] event selection:");
    println!("  first={first_event_time:?}");
    println!("  last={last_event_time:?}");
    println!("  expected_start={expected_start:?}");
    println!("  target_end={target_end:?}");
    println!("  needed={needed}");
    println!("[CLIP] stable segments chronological order:");
    for segment in segments {
        println!("  index={} modified={:?}", segment.index, segment.modified);
    }
    tracing::debug!(
        first_event_time = ?first_event_time,
        last_event_time = ?last_event_time,
        target_end = ?target_end,
        expected_start = ?expected_start,
        available_segment_indexes = ?segments.iter().map(|segment| segment.index).collect::<Vec<_>>(),
        available_segment_modified_times = ?segments.iter().map(|segment| segment.modified).collect::<Vec<_>>(),
        needed,
        "selecting replay segments around event"
    );
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

    fn segment_at(index: u64, seconds: u64, millis: u64) -> ReplaySegment {
        ReplaySegment {
            index,
            path: PathBuf::from(segment_file_name(index)),
            modified: SystemTime::UNIX_EPOCH
                + Duration::from_secs(seconds)
                + Duration::from_millis(millis),
        }
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

        assert_eq!(snapshot.selected.len(), 3);
        assert!(snapshot
            .selected
            .windows(2)
            .all(|window| (window[0].modified, window[0].index)
                <= (window[1].modified, window[1].index)));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selected_duration_can_cover_clip_seconds() {
        assert!(selected_segments_duration(10, 2) >= Duration::from_secs(20));
    }

    #[test]
    fn computes_export_segments_needed_from_duration() {
        assert_eq!(segments_needed_for_duration(25, 5), 5);
        assert_eq!(segments_needed_for_duration(20, 5), 4);
        assert_eq!(segments_needed_for_duration(25, 2), 13);
    }

    #[test]
    fn selects_only_requested_duration_segments() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (0..8)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs(index * 5),
            })
            .collect::<Vec<_>>();

        let selected = select_segments_for_duration(&segments, 25, 5);

        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![3, 4, 5, 6, 7]
        );
    }

    #[test]
    fn selects_segments_around_event_time() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (0..10)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = match select_segments_around_event(
            &segments,
            base + Duration::from_secs(27),
            base + Duration::from_secs(27),
            25,
            6,
            5,
        ) {
            EventSegmentSelection::Selected(selected) => selected,
            other => panic!("expected selected segments, got {other:?}"),
        };

        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn returns_none_when_event_is_not_covered_by_buffer() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (0..5)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = select_segments_around_event(
            &segments,
            base + Duration::from_secs(60),
            base + Duration::from_secs(60),
            25,
            6,
            5,
        );

        assert!(matches!(
            selected,
            EventSegmentSelection::NotReadyYet { .. }
        ));
    }

    #[test]
    fn returns_not_ready_when_post_event_end_is_not_stable_yet() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (0..5)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = select_segments_around_event(
            &segments,
            base + Duration::from_secs(20),
            base + Duration::from_secs(20),
            25,
            10,
            5,
        );

        assert!(matches!(
            selected,
            EventSegmentSelection::NotReadyYet { .. }
        ));
    }

    #[test]
    fn returns_too_old_when_required_start_left_buffer() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (10..15)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = select_segments_around_event(
            &segments,
            base + Duration::from_secs(20),
            base + Duration::from_secs(20),
            25,
            5,
            5,
        );

        assert!(matches!(selected, EventSegmentSelection::TooOld { .. }));
    }

    #[test]
    fn selects_exactly_five_segments_for_twenty_five_second_clip() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let segments = (0..8)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = match select_segments_around_event(
            &segments,
            base + Duration::from_secs(20),
            base + Duration::from_secs(20),
            25,
            5,
            5,
        ) {
            EventSegmentSelection::Selected(selected) => selected,
            other => panic!("expected selected segments, got {other:?}"),
        };

        assert_eq!(selected.len(), 5);
        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn multi_kill_selection_can_exceed_single_clip_duration() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let segments = (0..10)
            .map(|index| ReplaySegment {
                index,
                path: PathBuf::from(segment_file_name(index)),
                modified: base + Duration::from_secs((index + 1) * 5),
            })
            .collect::<Vec<_>>();

        let selected = match select_segments_around_event(
            &segments,
            base + Duration::from_secs(25),
            base + Duration::from_secs(35),
            25,
            5,
            5,
        ) {
            EventSegmentSelection::Selected(selected) => selected,
            other => panic!("expected long multi-kill selection, got {other:?}"),
        };

        assert_eq!(selected.len(), 7);
        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7]
        );
    }

    #[test]
    fn selects_wrapped_splitmux_segments_by_modified_time_case_one() {
        let segments = vec![
            segment_at(0, 2039, 469),
            segment_at(1, 2044, 464),
            segment_at(2, 2049, 467),
            segment_at(3, 2054, 469),
            segment_at(5, 2029, 466),
        ];
        let event_time =
            SystemTime::UNIX_EPOCH + Duration::from_secs(2044) + Duration::from_millis(749);

        let selected =
            match select_segments_around_event(&segments, event_time, event_time, 25, 5, 5) {
                EventSegmentSelection::Selected(selected) => selected,
                other => panic!("expected wrapped segments to be selectable, got {other:?}"),
            };

        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![5, 0, 1, 2, 3]
        );
    }

    #[test]
    fn selects_wrapped_splitmux_segments_by_modified_time_case_two() {
        let segments = vec![
            segment_at(0, 2074, 462),
            segment_at(1, 2079, 474),
            segment_at(2, 2084, 464),
            segment_at(4, 2059, 461),
            segment_at(5, 2064, 468),
        ];
        let event_time =
            SystemTime::UNIX_EPOCH + Duration::from_secs(2076) + Duration::from_millis(850);

        let selected =
            match select_segments_around_event(&segments, event_time, event_time, 25, 5, 5) {
                EventSegmentSelection::Selected(selected) => selected,
                other => panic!("expected wrapped segments to be selectable, got {other:?}"),
            };

        assert_eq!(
            selected
                .iter()
                .map(|segment| segment.index)
                .collect::<Vec<_>>(),
            vec![4, 5, 0, 1, 2]
        );
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
