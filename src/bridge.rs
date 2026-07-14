use std::{
    env,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result};

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
            let sidecar = sidecar_candidates(directory, name)
                .into_iter()
                .find(|candidate| candidate.is_file());
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

    pub fn spawn_server(&self) -> Result<Child> {
        let mut command = self.command()?;
        command
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command.spawn().with_context(|| {
            format!(
                "starting persistent BATSView bridge with {}",
                self.executable.display()
            )
        })
    }

    fn command(&self) -> Result<Command> {
        let mut command = Command::new(&self.executable);
        if let Some(script) = &self.script {
            command.arg(script);
        }
        if let Some(source) = &self.source {
            let mut paths = vec![source.clone()];
            if let Some(current) = env::var_os("PYTHONPATH") {
                paths.extend(env::split_paths(&current));
            }
            command.env("PYTHONPATH", env::join_paths(paths)?);
        }
        Ok(command)
    }
}

fn sidecar_candidates(executable_directory: &Path, name: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        executable_directory.join(name),
        executable_directory.join("bridge").join(name),
    ];

    // macOS application bundle: BATSView.app/Contents/MacOS/batsview
    candidates.push(executable_directory.join("../Resources/bridge").join(name));

    // AppImage and Debian package: usr/bin/batsview + usr/lib/batsview/bridge/...
    if let Some(usr) = executable_directory.parent() {
        candidates.push(usr.join("lib/batsview/bridge").join(name));
    }

    candidates
}

fn source_path() -> Option<PathBuf> {
    env::var_os("BATSPLOT_SOURCE")
        .map(PathBuf::from)
        .or_else(|| {
            let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../batsplot/src");
            sibling.is_dir().then_some(sibling)
        })
}

#[cfg(test)]
mod tests {
    use super::sidecar_candidates;
    use std::path::Path;

    #[test]
    fn packaged_sidecar_locations_are_considered() {
        let candidates =
            sidecar_candidates(Path::new("/opt/BATSView.app/Contents/MacOS"), "bridge");
        assert!(candidates.contains(&Path::new("/opt/BATSView.app/Contents/MacOS/bridge").into()));
        assert!(candidates.contains(
            &Path::new("/opt/BATSView.app/Contents/MacOS/../Resources/bridge/bridge").into()
        ));

        let candidates = sidecar_candidates(Path::new("/usr/bin"), "bridge");
        assert!(candidates.contains(&Path::new("/usr/lib/batsview/bridge/bridge").into()));
    }
}
