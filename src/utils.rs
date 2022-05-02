use std::env;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use nix::unistd::{access, AccessFlags};


/// Returns full path of the given program `name`.
///
/// If `name` doesn't contain path separators, looks for an executable file
/// named `name` in the directories listed in the environment variable `PATH`
/// and if found, returns its full path.
///
/// If `name` is an absolute path, returns it if the file exists and is
/// executable.
///
/// Otherwise, returns an Error.
pub fn which(name: &str) -> Result<String> {
    let name_path = Path::new(name);

    if name_path.components().count() == 1 {
        let paths = env::var("PATH").expect("Environment variable PATH is not defined"); // panics!

        env::split_paths(&paths)
            .find_map(|mut path| {
                path.push(name_path);
                if path.is_file() && access(&path, AccessFlags::X_OK).is_ok() {
                    Some(path.to_string_lossy().into())
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow!("Command was not found on PATH: {}", name))

    } else if name_path.is_file() && access(name_path, AccessFlags::X_OK).is_ok() {
        Ok(name.to_string())

    } else {
        bail!("File does not exist or is not executable: {}", name)
    }
}
