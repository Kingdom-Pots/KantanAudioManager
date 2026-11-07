//! App preferences that apply to every project, kept in a small text file.

use std::path::{Path, PathBuf};

use crate::timeline::TrackSettings;

/// Where preferences are kept. In the browser this names a storage key instead of a file.
pub fn path() -> Option<PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return Some(PathBuf::from(STORAGE_KEY));
    }
    crate::session::data_dir().map(|dir| dir.join("preferences.txt"))
}

const STORAGE_KEY: &str = "kantan-audio-manager-preferences";

fn read(path: &Path) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        crate::web::stored(STORAGE_KEY)
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::fs::read_to_string(path).ok()
}

/// Reads saved settings, falling back to defaults for anything missing or invalid.
pub fn load(path: &Path) -> TrackSettings {
    let mut settings = TrackSettings::default();
    let Some(text) = read(path) else { return settings };
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else { continue };
        let Ok(value) = value.trim().parse::<f32>() else { continue };
        match name.trim() {
            "visible_seconds" => settings.visible_seconds = value,
            "clip_height" => settings.clip_height = value,
            _ => {}
        }
    }
    settings.clamped()
}

/// A failure only means the settings won't be remembered next time.
pub fn save(path: &Path, settings: &TrackSettings) {
    let text = format!(
        "visible_seconds={}\nclip_height={}\n",
        settings.visible_seconds, settings.clip_height
    );
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        crate::web::store(STORAGE_KEY, &text);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_bad_values_fall_back() {
        let dir = std::env::temp_dir().join(format!("kantan_prefs_{}", std::process::id()));
        let path = dir.join("preferences.txt");
        let settings = TrackSettings { visible_seconds: 45.0, clip_height: 240.0 };
        save(&path, &settings);
        assert_eq!(load(&path), settings);

        std::fs::write(&path, "visible_seconds=nonsense\nclip_height=99999\nother=1\n").unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.visible_seconds, TrackSettings::default().visible_seconds);
        assert_eq!(loaded.clip_height, TrackSettings::MAX_CLIP_HEIGHT);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(load(&dir), TrackSettings::default(), "a missing file gives defaults");
    }
}
