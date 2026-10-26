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

/// One encoded video frame (an Annex-B access unit) for tiles that decode in the WebView with WebCodecs.
pub struct Raw {
    pub hevc: bool,
    pub key: bool,
    pub data: Vec<u8>,
}

/// Frames a WebCodecs viewer falls this far behind on are dropped; it resyncs on the next keyframe.
const RAW_QUEUE: usize = 64;
/// Longest group of pictures kept for new viewers; beyond this, caching waits for the next keyframe.
const GOP_MAX: usize = 600;

/// A camera's encoded frames for any number of WebCodecs viewers.
pub struct RawFeed {
    tx: tokio::sync::broadcast::Sender<std::sync::Arc<Raw>>,
    /// Frames since the last keyframe, so a new viewer shows a picture at once instead of at the next keyframe.
    gop: std::sync::Mutex<Vec<std::sync::Arc<Raw>>>,
}

impl Default for RawFeed {
    fn default() -> Self {
        Self { tx: tokio::sync::broadcast::channel(RAW_QUEUE).0, gop: Default::default() }
    }
}

impl RawFeed {
    pub fn push(&self, frame: Raw) {
        let frame = std::sync::Arc::new(frame);
        let mut gop = self.gop.lock().unwrap();
        if frame.key {
            gop.clear();
        }
        if (frame.key || !gop.is_empty()) && gop.len() < GOP_MAX {
            gop.push(frame.clone());
        } else {
            gop.clear();
        }
        // Sent under the lock, so a viewer joining in `subscribe` gets each frame exactly once.
        let _ = self.tx.send(frame);
    }

    /// The stream stopped: new viewers mustn't start from a stale picture.
    pub fn clear(&self) {
        self.gop.lock().unwrap().clear();
    }

    /// The current group of pictures, and every frame after it.
    pub fn subscribe(&self) -> (Vec<std::sync::Arc<Raw>>, tokio::sync::broadcast::Receiver<std::sync::Arc<Raw>>) {
        let gop = self.gop.lock().unwrap();
        (gop.clone(), self.tx.subscribe())
    }
}

/// Video a recording may have waiting for ffmpeg (a minute or more of a main stream) before frames are dropped.
const REC_QUEUE_BYTES: usize = 64 << 20;

/// What became of a frame handed to a [`Recorder`].
#[derive(Debug, PartialEq)]
pub enum Pushed {
    /// Queued for ffmpeg (or skipped while waiting for the first keyframe).
    Queued,
    /// ffmpeg is too far behind (slow or stalled disk): frames are being dropped until it catches up, and the
    /// recording resumes at a keyframe. Returned once when dropping starts.
    Overloaded,
    /// ffmpeg has gone (disk full, crashed): nothing more is being written.
    Dead,
}

pub struct Recorder {
    pub path: PathBuf,
    child: Option<Child>,
    tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    queued: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Waiting for a keyframe: at the start, and after dropping frames.
    waiting: bool,
    dropping: bool,
}

impl Recorder {
    /// Record into `dir` as `<base>_<YYYY-MM-DD_HH-MM-SS>.mp4`, starting a new file every `segment_minutes`
    /// (0 = one file). Fragmented MP4 (a fragment per keyframe): a crash loses only the last fragment, and closing
    /// a segment never rewrites it the way faststart did. WebView2 plays and seeks these over the asset protocol.
    pub fn new(codec: VideoType, dir: &Path, base: &str, segment_minutes: u32) -> std::io::Result<Self> {
        let template = dir.join(format!("{base}_%Y-%m-%d_%H-%M-%S.mp4")).to_string_lossy().into_owned();
        let seconds = if segment_minutes == 0 { 360_000 } else { segment_minutes * 60 }.to_string();
        let mut child = spawn(
            &[
                "-use_wallclock_as_timestamps", "1", "-f", format_of(codec), "-i", "pipe:0",
                "-c", "copy", "-f", "segment", "-segment_time", &seconds, "-reset_timestamps", "1",
                "-strftime", "1", "-segment_format", "mp4", "-segment_format_options", "movflags=+frag_keyframe+empty_moov+default_base_moof", &template,
            ],
            false,
        )?;
        let stdin = child.stdin.take().expect("piped stdin");
        Ok(Self::with_writer(dir.to_owned(), Some(child), stdin))
    }

    /// The writer thread owns all waiting on the disk; the camera loop only ever queues or drops.
    fn with_writer(path: PathBuf, child: Option<Child>, mut out: impl Write + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let queued = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let q = queued.clone();
        std::thread::spawn(move || {
            for buf in rx {
                if out.write_all(&buf).is_err() {
                    break;
                }
                q.fetch_sub(buf.len(), std::sync::atomic::Ordering::Relaxed);
            }
        });
        Self { path, child, tx: Some(tx), queued, waiting: true, dropping: false }
    }

    /// Never blocks: a stalled ffmpeg must not stall the camera's media, commands and pings.
    pub fn push(&mut self, data: &[u8], keyframe: bool) -> Pushed {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(tx) = &self.tx else { return Pushed::Dead };
        if self.queued.load(Relaxed) + data.len() > REC_QUEUE_BYTES {
            self.waiting = true; // a file can only pick up again at a keyframe
            return if std::mem::replace(&mut self.dropping, true) { Pushed::Queued } else { Pushed::Overloaded };
        }
        if self.waiting && !keyframe {
            return Pushed::Queued;
        }
        self.waiting = false;
        self.dropping = false;
        self.queued.fetch_add(data.len(), Relaxed);
        if tx.send(data.to_vec()).is_err() {
            return Pushed::Dead; // the writer stopped: ffmpeg quit
        }
        Pushed::Queued
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
    fn recorder_reports_when_ffmpeg_is_gone() {
        let dir = std::env::temp_dir().join("reolink-rs-rec-gone");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut rec = Recorder::new(VideoType::H264, &dir, "Cam", 0).unwrap();
        assert_eq!(rec.push(b"\0\0\0\x01", true), Pushed::Queued);
        let mut child = rec.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        // The feeder notices on its next write; from then on every push fails.
        let gone = (0..500).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            rec.push(&[0u8; 4096], false) == Pushed::Dead
        });
        assert!(gone, "push kept succeeding after ffmpeg exited");
        drop(rec);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn raw_viewers_start_at_the_last_keyframe() {
        let feed = RawFeed::default();
        let f = |key, n: u8| Raw { hevc: true, key, data: vec![n] };
        feed.push(f(false, 0)); // before any keyframe: nothing to start from
        assert!(feed.subscribe().0.is_empty());
        feed.push(f(true, 1));
        feed.push(f(false, 2));
        feed.push(f(true, 3));
        feed.push(f(false, 4));
        let (gop, mut rx) = feed.subscribe();
        assert_eq!(gop.iter().map(|r| r.data[0]).collect::<Vec<_>>(), [3, 4]);
        feed.push(f(false, 5));
        assert_eq!(rx.try_recv().unwrap().data, [5], "live frames follow the cached ones without a gap");
        feed.clear();
        assert!(feed.subscribe().0.is_empty());
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

    /// A writer that blocks until the test lets it go, then records what reached it.
    struct Gate(std::sync::mpsc::Receiver<()>, std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Gate {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.recv().map_err(|_| std::io::ErrorKind::BrokenPipe)?;
            self.1.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn recorder_never_blocks_and_resumes_at_a_keyframe() {
        let (open, gate) = std::sync::mpsc::channel();
        let got = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let mut r = Recorder::with_writer(PathBuf::new(), None, Gate(gate, got.clone()));
        let mb = vec![1u8; 1 << 20];
        assert_eq!(r.push(&[9], false), Pushed::Queued); // before the first keyframe: skipped
        // A stalled disk: the queue fills, then frames drop with one report, all without blocking.
        let t = std::time::Instant::now();
        let results: Vec<_> = (0..80).map(|i| r.push(&mb, i == 0)).collect();
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(results.iter().filter(|p| **p == Pushed::Overloaded).count(), 1);
        assert_eq!(results.iter().position(|p| *p == Pushed::Overloaded), Some(REC_QUEUE_BYTES >> 20));
        // The disk catches up; a delta frame still waits, the next keyframe is written.
        for _ in 0..(REC_QUEUE_BYTES >> 20) {
            open.send(()).unwrap();
        }
        while r.queued.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            std::thread::yield_now();
        }
        assert_eq!(r.push(&[2], false), Pushed::Queued);
        assert_eq!(r.push(&[3], true), Pushed::Queued);
        open.send(()).unwrap();
        while r.queued.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            std::thread::yield_now();
        }
        let got = got.lock().unwrap();
        assert_eq!(got.len(), REC_QUEUE_BYTES + 1);
        assert_eq!(got.last(), Some(&3));
    }
}
