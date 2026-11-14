//! What the frontend remembers between runs: the games folder picked in the
//! menu, kept as one line of text in the user's configuration folder.

use std::path::{Path, PathBuf};

/// The file holding the games folder, given the environment's variables:
/// `%APPDATA%` on Windows, otherwise `$XDG_CONFIG_HOME` or `~/.config`.
fn settings_file(var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let base = var("APPDATA").or_else(|| var("XDG_CONFIG_HOME")).or_else(|| var("HOME").map(|home| home.join(".config")))?;
    Some(base.join("yu-emulator").join("games-folder"))
}

fn env_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn read(file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let folder = text.trim_end_matches(['\r', '\n']);
    (!folder.is_empty()).then(|| PathBuf::from(folder))
}

fn write(file: &Path, folder: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("could not write {}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(file, format!("{}\n", folder.display())).map_err(failed)
}

/// The games folder saved by an earlier run, if any.
pub fn games_folder() -> Option<PathBuf> {
    read(&settings_file(env_var)?)
}

/// Remembers the games folder for the next runs.
pub fn save_games_folder(folder: &Path) -> Result<(), String> {
    let file = settings_file(env_var).ok_or("no configuration folder to remember the games folder in")?;
    write(&file, folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_live_in_the_configuration_folder() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| PathBuf::from(value))
        };
        assert_eq!(settings_file(env(&[("HOME", "/home/a")])), Some(PathBuf::from("/home/a/.config/yu-emulator/games-folder")));
        assert_eq!(
            settings_file(env(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/cfg")])),
            Some(PathBuf::from("/cfg/yu-emulator/games-folder"))
        );
        assert_eq!(settings_file(env(&[("APPDATA", "/roaming"), ("HOME", "/home/a")])), Some(PathBuf::from("/roaming/yu-emulator/games-folder")));
        assert_eq!(settings_file(env(&[])), None);
    }

    #[test]
    fn the_games_folder_is_written_and_read_back() {
        let root = std::env::temp_dir().join(format!("yu-emulator-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join("nested").join("games-folder");
        assert_eq!(read(&file), None);
        write(&file, Path::new("/games/Play Station")).unwrap();
        assert_eq!(read(&file), Some(PathBuf::from("/games/Play Station")));
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(read(&file), None, "an empty file names no folder");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
