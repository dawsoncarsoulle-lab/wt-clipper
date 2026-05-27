use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Local;

pub fn resolve_output_path_in_dir(
    output: Option<PathBuf>,
    output_dir: PathBuf,
) -> anyhow::Result<PathBuf> {
    match output {
        Some(path) => ensure_unique_path(path),
        None => {
            fs::create_dir_all(&output_dir)?;
            ensure_unique_path(output_dir.join(default_file_name()))
        }
    }
}

pub fn default_output_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; pass --output explicitly"))?;

    Ok(home.join("Videos").join("WarThunder Clips"))
}

pub fn default_file_name() -> String {
    format!("manual-{}.webm", Local::now().format("%Y-%m-%d-%H-%M-%S"))
}

pub fn slugify_filename_part(input: &str) -> String {
    const MAX_LEN: usize = 40;

    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in input.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }

        if slug.len() >= MAX_LEN {
            break;
        }
    }

    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "unknown".to_owned()
    } else {
        slug
    }
}

pub fn ensure_unique_path(path: PathBuf) -> anyhow::Result<PathBuf> {
    if !path.exists() {
        return Ok(path);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid output file name: {}", path.display()))?;
    let extension = path.extension().and_then(|value| value.to_str());

    for index in 1..10_000 {
        let file_name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!("could not find a unique output path for {}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_name_is_manual_webm() {
        let name = default_file_name();

        assert!(name.starts_with("manual-"));
        assert!(name.ends_with(".webm"));
    }

    #[test]
    fn default_output_dir_uses_home_videos_folder() {
        let dir = default_output_dir().expect("HOME should be set in tests");

        assert!(dir.ends_with(Path::new("Videos").join("WarThunder Clips")));
    }

    #[test]
    fn ensure_unique_path_returns_original_when_available() {
        let path = std::env::temp_dir().join(format!(
            "wt-clipper-test-{}-available.webm",
            std::process::id()
        ));

        assert_eq!(ensure_unique_path(path.clone()).unwrap(), path);
    }

    #[test]
    fn ensure_unique_path_adds_suffix_when_file_exists() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "wt-clipper-test-{}-exists.webm",
            std::process::id()
        ));
        fs::write(&path, b"exists").unwrap();

        let unique = ensure_unique_path(path.clone()).unwrap();

        assert_ne!(unique, path);
        assert!(unique
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .contains("-1.webm"));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn slugifies_aircraft_name() {
        assert_eq!(slugify_filename_part("F/A-18C Early"), "f-a-18c-early");
    }

    #[test]
    fn slugifies_symbolic_ground_vehicle_name() {
        assert_eq!(slugify_filename_part("◍M1A1 HC"), "m1a1-hc");
    }
}
