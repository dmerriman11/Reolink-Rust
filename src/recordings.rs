//! Playback side of local recording: list the MP4 segments in the recording folder, export clips, grab stills.
//! The UI plays files straight from disk through Tauri's asset protocol.

use serde::Serialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
pub struct File {
    pub name: String,
    pub size: u64,
    /// Last write, ms since the epoch: a finished segment's end time.
    pub modified: u64,
}

#[derive(Serialize)]
pub struct Listing {
    pub dir: PathBuf,
    pub files: Vec<File>,
}

/// The `.mp4` files directly in `dir` (exported clips live in a subfolder, so they don't show as recordings).
pub fn list(dir: &Path) -> Listing {
    let files = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
            (meta.is_file() && name.to_ascii_lowercase().ends_with(".mp4")).then_some(File { name, size: meta.len(), modified })
        })
        .collect();
    Listing { dir: dir.to_owned(), files }
}

/// Resolve a file name from the UI to a recording in `dir`, refusing anything that could point elsewhere.
pub fn resolve(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty() && !name.contains(['/', '\\', ':']) && !name.starts_with('.');
    if !plain || !name.to_ascii_lowercase().ends_with(".mp4") {
        return Err("not a recording".into());
    }
    let path = dir.join(name);
    if !path.is_file() {
        return Err("recording not found".into());
    }
    Ok(path)
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

/// "+01m05s" for an offset into a recording, used in exported file names.
fn offset_label(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("+{:02}m{:02}s", s / 60, s % 60)
}

/// Copy `from`..`to` seconds of `src` into `<dir>/Clips/` without re-encoding (starts at the keyframe before `from`).
pub async fn export_clip(src: &Path, dir: &Path, from: f64, to: f64) -> Result<PathBuf, String> {
    if !(from >= 0.0 && to > from) {
        return Err("clip end must be after its start".into());
    }
    let out_dir = dir.join("Clips");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("can't create {}: {e}", out_dir.display()))?;
    let out = out_dir.join(format!("{}_{}_{}.mp4", stem(src), offset_label(from), offset_label(to)));
    let (from, to) = (format!("{from:.3}"), format!("{to:.3}"));
    let o = OsStr::new;
    crate::media::run(&[o("-y"), o("-ss"), o(&from), o("-to"), o(&to), o("-i"), src.as_os_str(), o("-c"), o("copy"), o("-movflags"), o("+faststart"), out.as_os_str()]).await?;
    Ok(out)
}

/// Save the frame at `at` seconds into `src` as a JPEG in `dir`.
pub async fn still(src: &Path, dir: &Path, at: f64) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let out = dir.join(format!("{}_{}.jpg", stem(src), offset_label(at)));
    let at = format!("{:.3}", at.max(0.0));
    let o = OsStr::new;
    crate::media::run(&[o("-y"), o("-ss"), o(&at), o("-i"), src.as_os_str(), o("-frames:v"), o("1"), o("-q:v"), o("2"), out.as_os_str()]).await?;
    Ok(out)
}

/// Thumbnails are cached here, one `<recording>.jpg` each.
pub fn thumb_dir(dir: &Path) -> PathBuf {
    dir.join(".thumbs")
}

/// A small JPEG from about a second into `src`, made once and cached in thumb_dir.
pub async fn thumb(src: &Path, dir: &Path) -> Result<PathBuf, String> {
    let thumbs = thumb_dir(dir);
    let out = thumbs.join(format!("{}.jpg", stem(src)));
    if std::fs::metadata(&out).is_ok_and(|m| m.len() > 0) {
        return Ok(out);
    }
    std::fs::create_dir_all(&thumbs).map_err(|e| format!("can't create {}: {e}", thumbs.display()))?;
    let o = OsStr::new;
    let res = crate::media::run(&[o("-y"), o("-ss"), o("1"), o("-i"), src.as_os_str(), o("-frames:v"), o("1"), o("-vf"), o("scale=320:-2"), o("-q:v"), o("5"), out.as_os_str()]).await;
    if res.is_err() || !out.is_file() {
        // Still recording or cut off: no cache entry, so a later call can try again.
        let _ = std::fs::remove_file(&out);
        return Err(res.err().unwrap_or_else(|| "no frame to show".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_paths_outside_the_folder() {
        let dir = std::env::temp_dir().join("reolink-rs-rec-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cam_2026-10-25_10-23-49.mp4"), b"").unwrap();
        assert!(resolve(&dir, "Cam_2026-10-25_10-23-49.mp4").is_ok());
        for bad in ["../x.mp4", "..\\x.mp4", "C:x.mp4", "sub/x.mp4", ".mp4", "notes.txt", "missing.mp4", ""] {
            assert!(resolve(&dir, bad).is_err(), "{bad}");
        }
        let names: Vec<_> = list(&dir).files.into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["Cam_2026-10-25_10-23-49.mp4"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn thumbnails_are_made_once_and_cached() {
        let dir = std::env::temp_dir().join("reolink-rs-thumb-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("Cam_2026-10-25_10-00-00.mp4");
        let o = OsStr::new;
        crate::media::run(&[o("-f"), o("lavfi"), o("-i"), o("testsrc=size=640x360:rate=10:duration=3"), src.as_os_str()]).await.unwrap();
        let t = thumb(&src, &dir).await.unwrap();
        assert_eq!(t, thumb_dir(&dir).join("Cam_2026-10-25_10-00-00.jpg"));
        let made = std::fs::metadata(&t).unwrap().modified().unwrap();
        assert_eq!(thumb(&src, &dir).await.unwrap(), t);
        assert_eq!(std::fs::metadata(&t).unwrap().modified().unwrap(), made, "second call reuses the cache");
        // A file with no frames (like one still being written) gives an error and leaves no cache entry behind.
        let broken = dir.join("Cam_2026-10-25_11-00-00.mp4");
        std::fs::write(&broken, b"not a video").unwrap();
        assert!(thumb(&broken, &dir).await.is_err());
        assert!(!thumb_dir(&dir).join("Cam_2026-10-25_11-00-00.jpg").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offsets_read_as_minutes_and_seconds() {
        assert_eq!(offset_label(65.9), "+01m05s");
        assert_eq!(offset_label(-3.0), "+00m00s");
    }
}
