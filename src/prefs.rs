//! App (local) settings, mirroring the official client's Local Settings page where it applies.
//! Held in memory for the device workers and persisted with the device list.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Allowed recording segment lengths in minutes; 0 keeps one file per recording.
pub const SEGMENT_MINUTES: [u32; 6] = [0, 5, 10, 15, 30, 60];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub snapshot_dir: PathBuf,
    pub record_dir: PathBuf,
    /// Split recordings into files of this many minutes (0 = one file). Segments limit what a crash can lose.
    pub segment_minutes: u32,
    /// Warn while recording when the recording drive has less free space than this (GB, 0 = off).
    pub space_warning_gb: u32,
    pub hardware_decode: bool,
    pub run_at_startup: bool,
    /// Look for new cameras on the LAN automatically.
    pub scan_network: bool,
    /// Beep when a camera reports motion (official "Alarm beep"), at this volume 0-100.
    pub alarm_beep: bool,
    pub alarm_volume: u32,
    /// Local recording without pressing Record (official "Local record schedule"): "off", "motion", "continuous".
    pub local_record: String,
    /// Hours it applies: 7x24 '0'/'1', Sunday first, local time.
    pub record_schedule: String,
}

pub const LOCAL_RECORD: [&str; 3] = ["off", "motion", "continuous"];

/// Seconds a motion recording continues after the camera reports the motion has stopped.
pub const MOTION_POST_SECS: u64 = 30;

impl Default for Prefs {
    fn default() -> Self {
        let media = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("Videos").join("Reolink-rs");
        Prefs {
            snapshot_dir: media.clone(),
            record_dir: media,
            segment_minutes: 30,
            space_warning_gb: 5,
            hardware_decode: true,
            run_at_startup: false,
            scan_network: true,
            alarm_beep: false,
            alarm_volume: 60,
            local_record: "off".into(),
            record_schedule: "1".repeat(168),
        }
    }
}

static CURRENT: RwLock<Option<Prefs>> = RwLock::new(None);

pub fn get() -> Prefs {
    CURRENT.read().unwrap().clone().unwrap_or_default()
}

pub fn set(p: Prefs) {
    *CURRENT.write().unwrap() = Some(p);
}

impl Prefs {
    /// Reject values the app can't use, and make sure both folders exist and are writable.
    pub fn validate(&self) -> Result<(), String> {
        if !SEGMENT_MINUTES.contains(&self.segment_minutes) {
            return Err("Video file duration must be one of the listed options".into());
        }
        if self.alarm_volume > 100 {
            return Err("Alarm volume must be 0-100".into());
        }
        if !LOCAL_RECORD.contains(&self.local_record.as_str()) {
            return Err("Local recording must be off, on motion or continuous".into());
        }
        if self.record_schedule.len() != 168 || !self.record_schedule.chars().all(|c| c == '0' || c == '1') {
            return Err("Recording schedule must cover 7 days x 24 hours".into());
        }
        if self.space_warning_gb > 10_000 {
            return Err("Space warning must be 10,000 GB or less".into());
        }
        for (label, dir) in [("Snapshot folder", &self.snapshot_dir), ("Recording folder", &self.record_dir)] {
            if !dir.is_absolute() {
                return Err(format!("{label} must be a full path"));
            }
            std::fs::create_dir_all(dir).map_err(|e| format!("{label}: can't create {}: {e}", dir.display()))?;
            let probe = dir.join(".reolink-rs-write-test");
            std::fs::write(&probe, b"").map_err(|e| format!("{label}: can't write to {}: {e}", dir.display()))?;
            let _ = std::fs::remove_file(probe);
        }
        Ok(())
    }
}

impl Prefs {
    /// Whether local recording applies at `weekday` (0 = Sunday) and `hour`.
    pub fn scheduled(&self, weekday: u8, hour: u8) -> bool {
        self.local_record != "off" && self.record_schedule.as_bytes().get(weekday as usize * 24 + hour as usize) == Some(&b'1')
    }
}

/// Free bytes on the volume holding `path`.
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: NUL-terminated UTF-16 path; out-pointer is a live u64; the other outputs are optional.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Add or remove the per-user "run at startup" entry for this executable.
pub fn apply_run_at_startup(enable: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("reg");
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        cmd.args(["add", RUN_KEY, "/v", "Reolink-rs", "/t", "REG_SZ", "/f", "/d"]).arg(format!("\"{}\"", exe.display()));
    } else {
        cmd.args(["delete", RUN_KEY, "/v", "Reolink-rs", "/f"]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("couldn't run reg.exe: {e}"))?;
    // Deleting an entry that isn't there is fine.
    if out.status.success() || !enable {
        Ok(())
    } else {
        Err(format!("couldn't update startup entry: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_old_configs_load() {
        let d = Prefs::default();
        assert!(SEGMENT_MINUTES.contains(&d.segment_minutes));
        // A config saved before a field existed still loads, filling defaults.
        let p: Prefs = serde_json::from_str(r#"{"hardware_decode": false}"#).unwrap();
        assert!(!p.hardware_decode);
        assert_eq!(p.segment_minutes, d.segment_minutes);
    }

    #[test]
    fn validation_rejects_bad_values() {
        let dir = std::env::temp_dir().join("reolink-rs-prefs-test");
        let ok = Prefs { snapshot_dir: dir.clone(), record_dir: dir.clone(), ..Prefs::default() };
        assert!(ok.validate().is_ok());
        assert!(Prefs { segment_minutes: 7, ..ok.clone() }.validate().is_err());
        assert!(Prefs { record_dir: "relative".into(), ..ok.clone() }.validate().is_err());
        assert!(Prefs { space_warning_gb: 20_000, ..ok.clone() }.validate().is_err());
        assert!(Prefs { alarm_volume: 101, ..ok.clone() }.validate().is_err());
        assert!(Prefs { local_record: "sometimes".into(), ..ok.clone() }.validate().is_err());
        assert!(Prefs { record_schedule: "1".repeat(167), ..ok }.validate().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schedule_hours_are_sunday_first() {
        let mut p = Prefs { local_record: "motion".into(), record_schedule: "0".repeat(168), ..Prefs::default() };
        p.record_schedule.replace_range(24 + 9..24 + 10, "1"); // Monday 09:00
        assert!(p.scheduled(1, 9) && !p.scheduled(1, 10) && !p.scheduled(0, 9));
        assert!(!Prefs { local_record: "off".into(), ..Prefs::default() }.scheduled(1, 9));
    }

    #[cfg(windows)]
    #[test]
    fn reports_free_space() {
        assert!(free_bytes(&std::env::temp_dir()).unwrap() > 0);
    }
}
