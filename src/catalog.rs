use std::{fs, path::Path};

use anyhow::{Context, Result};

use crate::protocol::{PlotFile, ScanResult};

pub fn scan_directory(directory: &Path, recursive: bool) -> Result<ScanResult> {
    let directory = directory
        .canonicalize()
        .with_context(|| format!("opening run directory {}", directory.display()))?;
    let mut files = Vec::new();
    let mut pending = vec![directory.clone()];

    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)
            .with_context(|| format!("reading run directory {}", current.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if recursive && file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file() || !has_plt_extension(&path) {
                continue;
            }

            let name = entry.file_name().to_string_lossy().into_owned();
            let (section, var_id, time_step, dump_index) = parse_filename(&name)
                .map(|parsed| {
                    (
                        Some(parsed.section),
                        Some(parsed.var_id),
                        Some(parsed.time_step),
                        Some(parsed.dump_index),
                    )
                })
                .unwrap_or((None, None, None, None));
            files.push(PlotFile {
                path: path.to_string_lossy().into_owned(),
                name,
                size: entry.metadata()?.len(),
                section,
                var_id,
                time_step,
                dump_index,
            });
        }
    }

    files.sort_by(|left, right| {
        left.section
            .cmp(&right.section)
            .then(left.var_id.cmp(&right.var_id))
            .then(left.time_step.cmp(&right.time_step))
            .then(left.dump_index.cmp(&right.dump_index))
            .then(left.name.cmp(&right.name))
    });
    Ok(ScanResult {
        protocol: 1,
        directory: directory.to_string_lossy().into_owned(),
        files,
    })
}

fn has_plt_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("plt"))
}

struct ParsedFilename {
    section: String,
    var_id: u64,
    time_step: u64,
    dump_index: u64,
}

fn parse_filename(name: &str) -> Option<ParsedFilename> {
    let extension = name.rsplit_once('.')?;
    if !extension.1.eq_ignore_ascii_case("plt") {
        return None;
    }
    let (prefix, dump_index) = extension.0.rsplit_once("_n")?;
    let (prefix, time_step) = prefix.rsplit_once("_t")?;
    let (section, var_id) = prefix.rsplit_once("_var_")?;
    let section = section.trim_matches('_');
    if section.is_empty() {
        return None;
    }
    Some(ParsedFilename {
        section: section.to_owned(),
        var_id: var_id.parse().ok()?,
        time_step: time_step.parse().ok()?,
        dump_index: dump_index.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_filename() {
        let parsed = parse_filename("z=0_var_3_t00000100_n00000042.plt").unwrap();
        assert_eq!(parsed.section, "z=0");
        assert_eq!(parsed.var_id, 3);
        assert_eq!(parsed.time_step, 100);
        assert_eq!(parsed.dump_index, 42);
    }

    #[test]
    fn scans_case_insensitive_extensions_and_optional_subdirectories() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("y=0_var_2_t00000003_n00000004.PLT"),
            b"abc",
        )
        .unwrap();
        fs::write(root.path().join("notes.txt"), b"ignored").unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("custom.plt"), b"x").unwrap();

        let flat = scan_directory(root.path(), false).unwrap();
        assert_eq!(flat.files.len(), 1);
        assert_eq!(flat.files[0].section.as_deref(), Some("y=0"));

        let recursive = scan_directory(root.path(), true).unwrap();
        assert_eq!(recursive.files.len(), 2);
        assert!(recursive.files.iter().any(|file| file.name == "custom.plt"));
    }
}
