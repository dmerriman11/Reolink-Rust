//! Video decode and MP4 recording via an ffmpeg subprocess.
//! shortcut: needs ffmpeg.exe on PATH or next to the exe; upgrade to linked libavcodec if that's a burden.

use neolink_core::bcmedia::model::VideoType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

/// Largest JPEG we produce; even a maximized single view rarely shows more than this.
const MAX_BOX: (u32, u32) = (2560, 1440);
/// Box sizes are rounded up to this step so window resizes don't constantly restart the decoder.
const BOX_STEP: u32 = 128;

/// The JPEG bounding box for a tile of `w`x`h` device pixels: rounded up to BOX_STEP, capped at MAX_BOX.
pub fn jpeg_box(w: u32, h: u32) -> (u32, u32) {
    let up = |v: u32, max: u32| v.max(1).div_ceil(BOX_STEP).saturating_mul(BOX_STEP).min(max);
    (up(w, MAX_BOX.0), up(h, MAX_BOX.1))
}

/// Before the UI reports a tile size.
pub const DEFAULT_BOX: (u32, u32) = (1280, 720);


fn ffmpeg() -> PathBuf {
    let local = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ffmpeg.exe")));
    // Apps launched from Finder don't get Homebrew's PATH.
    let brew = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"].map(PathBuf::from);
    match local {
        Some(p) if p.exists() => p,
        _ if cfg!(target_os = "macos") => brew.into_iter().find(|p| p.exists()).unwrap_or_else(|| "ffmpeg".into()),
        _ => PathBuf::from("ffmpeg"),
    }
}

fn format_of(codec: VideoType) -> &'static str {
    match codec {
        VideoType::H264 => "h264",
        VideoType::H265 => "hevc",
    }
}

fn spawn(args: &[&str], stdout: bool) -> std::io::Result<Child> {
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(if stdout { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
}

/// Run ffmpeg to completion (clip export, stills), returning its error output on failure.
pub async fn run(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error"]).args(args).stdin(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().await.map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Feeds Annex-B NAL units to a writer thread so a slow ffmpeg never stalls the network task.
fn feeder(mut stdin: ChildStdin) -> SyncSender<Vec<u8>> {
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        for buf in rx {
            if stdin.write_all(&buf).is_err() {
                break;
            }
        }
    });
    tx
}

pub struct Decoder {
    pub codec: VideoType,
    pub jpeg_box: (u32, u32),
    child: Child,
    tx: Option<SyncSender<Vec<u8>>>,
    waiting_for_key: bool,
}

impl Decoder {
    pub fn new(
        codec: VideoType,
        jpeg_box: (u32, u32),
        on_frame: impl Fn(Vec<u8>) + Send + 'static,
    ) -> std::io::Result<Self> {
        // JPEG frames carry their own size, so we never need the camera to tell us the resolution
        // (some, like the Argus 4 Pro, never send the stream info header).
        // Fit the tile's box (never upscaling): JPEG cost scales with pixels, so don't make more than is shown.
        let (w, h) = jpeg_box;
        let scale = format!("scale=w='min(iw,{w})':h='min(ih,{h})':force_original_aspect_ratio=decrease:force_divisible_by=2");
        let mut args: Vec<&str> = vec![];
        if crate::prefs::get().hardware_decode {
            args.extend(["-hwaccel", "auto"]); // GPU decode when available (~3x less CPU), silent software fallback
        }
        args.extend([
            "-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0",
            "-f", format_of(codec), "-i", "pipe:0",
            "-vf", &scale, "-c:v", "mjpeg", "-q:v", "4", "-f", "mpjpeg", "pipe:1",
        ]);
        let mut child = spawn(&args, true)?;
        let tx = feeder(child.stdin.take().expect("piped stdin"));
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        std::thread::spawn(move || {
            while let Some(frame) = read_mpjpeg(&mut stdout) {
                on_frame(frame);
            }
        });
        Ok(Self { codec, jpeg_box, child, tx: Some(tx), waiting_for_key: true })
    }

    pub fn push(&mut self, data: Vec<u8>, keyframe: bool) {
        if self.waiting_for_key && !keyframe {
            return;
        }
        self.waiting_for_key = false;
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(data)
        {
            // Dropped a frame; resync on the next keyframe instead of decoding garbage.
            self.waiting_for_key = true;
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.tx.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one part of ffmpeg's mpjpeg output: "--ffmpeg", headers incl. Content-length, blank line, JPEG.
fn read_mpjpeg(r: &mut impl std::io::BufRead) -> Option<Vec<u8>> {
    let mut len = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if let Some((k, v)) = l.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse::<usize>().ok();
        } else if l.is_empty() && len.is_some() {
            break;
        }
    }
    let mut jpeg = vec![0u8; len?];
    r.read_exact(&mut jpeg).ok()?;
    Some(jpeg)
}

pub struct Recorder {
    pub path: PathBuf,
    child: Option<Child>,
    tx: Option<SyncSender<Vec<u8>>>,
    started: bool,
}

impl Recorder {
    /// Record into `dir` as `<base>_<YYYY-MM-DD_HH-MM-SS>.mp4`, starting a new file every `segment_minutes`
    /// (0 = one file). Each segment is a complete MP4, so a crash loses at most the current one.
    pub fn new(codec: VideoType, dir: &Path, base: &str, segment_minutes: u32) -> std::io::Result<Self> {
        let template = dir.join(format!("{base}_%Y-%m-%d_%H-%M-%S.mp4")).to_string_lossy().into_owned();
        let seconds = if segment_minutes == 0 { 360_000 } else { segment_minutes * 60 }.to_string();
        let mut child = spawn(
            &[
                "-use_wallclock_as_timestamps", "1", "-f", format_of(codec), "-i", "pipe:0",
                "-c", "copy", "-f", "segment", "-segment_time", &seconds, "-reset_timestamps", "1",
                "-strftime", "1", "-segment_format", "mp4", "-segment_format_options", "movflags=+faststart", &template,
            ],
            false,
        )?;
        let tx = feeder(child.stdin.take().expect("piped stdin"));
        Ok(Self { path: dir.to_owned(), child: Some(child), tx: Some(tx), started: false })
    }

    pub fn push(&mut self, data: &[u8], keyframe: bool) {
        self.started |= keyframe;
        if self.started && let Some(tx) = &self.tx {
            // Blocking send: a recording must not silently lose frames.
            let _ = tx.send(data.to_vec());
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Closing stdin lets ffmpeg write the moov atom; wait off-thread so the UI never blocks.
        self.tx.take();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || child.wait());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a test pattern with ffmpeg and check the decoder hands back frames of the scaled size.
    fn roundtrip(codec: VideoType, enc: &str) {
        let out = Command::new(ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=2560x1440:rate=10", "-frames:v", "10", "-c:v", enc, "-f", format_of(codec), "pipe:1"])
            .output()
            .expect("ffmpeg on PATH");
        assert!(out.status.success() && !out.stdout.is_empty());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dec = Decoder::new(codec, (1920, 1080), move |jpeg| { let _ = tx.send(jpeg); }).unwrap();
        dec.push(out.stdout, true);
        dec.tx.take(); // EOF flushes the decoder
        let jpeg = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("a decoded frame");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI");
        // SOF0 marker carries the scaled size: 2560x1440 capped to 1920 wide.
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
        let h = u16::from_be_bytes([jpeg[sof + 5], jpeg[sof + 6]]);
        let w = u16::from_be_bytes([jpeg[sof + 7], jpeg[sof + 8]]);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn decodes_h264() {
        roundtrip(VideoType::H264, "libx264");
    }

    #[test]
    fn decodes_h265() {
        roundtrip(VideoType::H265, "libx265");
    }

    #[test]
    fn jpeg_box_rounds_up_and_caps() {
        assert_eq!(jpeg_box(900, 400), (1024, 512));
        assert_eq!(jpeg_box(1024, 512), (1024, 512));
        assert_eq!(jpeg_box(5000, 3000), (2560, 1440));
        assert_eq!(jpeg_box(0, 0), (128, 128));
    }

    #[test]
    fn parses_mpjpeg() {
        let data = b"--ffmpeg\r\nContent-type: image/jpeg\r\nContent-length: 3\r\n\r\nabc\r\n--ffmpeg\r\nContent-length: 2\r\n\r\nxy\r\n";
        let mut r = &data[..];
        assert_eq!(read_mpjpeg(&mut r).unwrap(), b"abc");
        assert_eq!(read_mpjpeg(&mut r).unwrap(), b"xy");
        assert!(read_mpjpeg(&mut r).is_none());
    }
}
