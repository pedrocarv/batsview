use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use crate::protocol::{FileInfo, ScanResult};

#[derive(Clone, Debug)]
pub struct Bridge {
    executable: PathBuf,
    script: Option<PathBuf>,
    source: Option<PathBuf>,
}

impl Bridge {
    pub fn discover() -> Self {
        if let Some(path) = env::var_os("BATSVIEW_BRIDGE") {
            return Self {
                executable: path.into(),
                script: None,
                source: source_path(),
            };
        }
        if let Ok(current) = env::current_exe() {
            let name = if cfg!(windows) {
                "batsview-bridge.exe"
            } else {
                "batsview-bridge"
            };
            let directory = current.parent().unwrap_or(Path::new("."));
            let standalone = directory.join(name);
            let bundled = directory.join("bridge").join(name);
            let sidecar = if standalone.is_file() {
                Some(standalone)
            } else if bundled.is_file() {
                Some(bundled)
            } else {
                None
            };
            if let Some(sidecar) = sidecar {
                return Self {
                    executable: sidecar,
                    script: None,
                    source: source_path(),
                };
            }
        }
        let python = env::var_os("BATSVIEW_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
        Self {
            executable: python,
            script: Some(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bridge/batsview_bridge.py"),
            ),
            source: source_path(),
        }
    }

    pub fn scan(&self, directory: &Path, recursive: bool) -> Result<ScanResult> {
        let mut arguments = vec![OsString::from("scan"), directory.as_os_str().to_owned()];
        if recursive {
            arguments.push(OsString::from("--recursive"));
        }
        self.json(&arguments)
    }

    pub fn inspect(&self, path: &Path) -> Result<FileInfo> {
        self.json(&[OsString::from("inspect"), path.as_os_str().to_owned()])
    }

    pub fn export(&self, path: &Path, variable: &str, output: &Path) -> Result<()> {
        let result = self.run(&[
            OsString::from("export"),
            path.as_os_str().to_owned(),
            OsString::from(variable),
            output.as_os_str().to_owned(),
        ])?;
        if !result.status.success() {
            bail!(bridge_error(&result));
        }
        Ok(())
    }

    fn json<T: DeserializeOwned>(&self, arguments: &[OsString]) -> Result<T> {
        let result = self.run(arguments)?;
        if !result.status.success() {
            bail!(bridge_error(&result));
        }
        serde_json::from_slice(&result.stdout).with_context(|| {
            format!(
                "invalid JSON from BATSView bridge: {}",
                String::from_utf8_lossy(&result.stdout)
            )
        })
    }

    fn run(&self, arguments: &[OsString]) -> Result<Output> {
        let mut command = Command::new(&self.executable);
        if let Some(script) = &self.script {
            command.arg(script);
        }
        command.args(arguments);
        if let Some(source) = &self.source {
            let mut paths = vec![source.clone()];
            if let Some(current) = env::var_os("PYTHONPATH") {
                paths.extend(env::split_paths(&current));
            }
            command.env("PYTHONPATH", env::join_paths(paths)?);
        }
        command.output().with_context(|| {
            format!(
                "starting BATSView bridge with {} (set BATSVIEW_BRIDGE or BATSVIEW_PYTHON to override)",
                self.executable.display()
            )
        })
    }
}

fn source_path() -> Option<PathBuf> {
    env::var_os("BATSPLOT_SOURCE")
        .map(PathBuf::from)
        .or_else(|| {
            let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../batsplot/src");
            sibling.is_dir().then_some(sibling)
        })
}

fn bridge_error(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(message) = value.get("error").and_then(|item| item.as_str())
    {
        return message.to_owned();
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        stderr.trim().to_owned()
    } else {
        text.trim().to_owned()
    }
}
