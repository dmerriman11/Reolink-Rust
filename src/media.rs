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


pub(crate) fn ffmpeg() -> PathBuf {
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
    /// Time on the camera's clock in microseconds, wrapping at u32.
    pub us: u32,
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

/// Wraps frames in MPEG-TS for ffmpeg: the one way into its `-c copy` that keeps Annex-B and carries a timestamp per
/// frame. A raw stream can only be stamped on arrival, which writes every network stall into the file.
struct Mpegts {
    stream_type: u8,
    /// Continuity counters for the PAT, the PMT and the video.
    counters: [u8; 3],
    last_us: Option<u32>,
    /// The last usable step between frames, for a clock that didn't move.
    step_us: u32,
    /// The camera's clock unwrapped. Starts above zero so ffmpeg never sees a time before the start.
    total_us: u64,
}

impl Mpegts {
    const PMT_PID: u16 = 0x1000;
    const VIDEO_PID: u16 = 0x0100;

    fn new(codec: VideoType) -> Self {
        let stream_type = match codec {
            VideoType::H264 => 0x1B,
            VideoType::H265 => 0x24,
        };
        Self { stream_type, counters: [0; 3], last_us: None, step_us: 66_667, total_us: 1_000_000 }
    }

    /// The 90 kHz timestamp for a frame stamped `us`; never goes backwards, as a file can't.
    fn pts(&mut self, us: u32) -> u64 {
        if let Some(last) = self.last_us {
            let delta = us.wrapping_sub(last) as i32; // signed, so the clock wrapping is just the next step
            if delta > 0 {
                self.step_us = delta as u32;
            }
            self.total_us += self.step_us as u64;
        }
        self.last_us = Some(us);
        (self.total_us * 9 / 100) & ((1 << 33) - 1)
    }

    /// One PSI table (PAT or PMT) in a packet of its own, with its CRC.
    fn table(&mut self, which: usize, pid: u16, section: &[u8], out: &mut Vec<u8>) {
        let mut p = [0xFF; 188];
        p[..5].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | self.counters[which], 0]);
        self.counters[which] = (self.counters[which] + 1) & 15;
        p[5..5 + section.len()].copy_from_slice(section);
        p[5 + section.len()..9 + section.len()].copy_from_slice(&mpeg_crc32(section).to_be_bytes());
        out.extend_from_slice(&p);
    }

    fn frame(&mut self, data: &[u8], key: bool, us: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((data.len() / 184 + 4) * 188);
        let pid = Self::VIDEO_PID.to_be_bytes();
        if key {
            // Before every keyframe, as a demuxer can only start from these.
            let pmt_pid = Self::PMT_PID.to_be_bytes();
            let pat = [0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE0 | pmt_pid[0], pmt_pid[1]];
            let pmt = [0x02, 0xB0, 18, 0, 1, 0xC1, 0, 0, 0xE0 | pid[0], pid[1], 0xF0, 0, self.stream_type, 0xE0 | pid[0], pid[1], 0xF0, 0];
            self.table(0, 0, &pat, &mut out);
            self.table(1, Self::PMT_PID, &pmt, &mut out);
        }
        let pts = self.pts(us);
        let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
        pes.extend_from_slice(&[
            0x21 | ((pts >> 29) & 0x0E) as u8,
            (pts >> 22) as u8,
            ((pts >> 14) & 0xFE) as u8 | 1,
            (pts >> 7) as u8,
            ((pts << 1) & 0xFE) as u8 | 1,
        ]);
        pes.extend_from_slice(data);
        let pcr = [(pts >> 25) as u8, (pts >> 17) as u8, (pts >> 9) as u8, (pts >> 1) as u8, ((pts & 1) << 7) as u8 | 0x7E, 0];

        let mut rest = &pes[..];
        let mut first = true;
        while !rest.is_empty() {
            // The first packet of a frame carries the clock; the last pads itself out to 188 with an adaptation field.
            let n = rest.len().min(if first { 176 } else { 184 });
            let field = 184 - n;
            let mut p = [0xFF; 188];
            p[..4].copy_from_slice(&[0x47, (first as u8) << 6 | pid[0], pid[1], if field > 0 { 0x30 } else { 0x10 } | self.counters[2]]);
            self.counters[2] = (self.counters[2] + 1) & 15;
            if field > 0 {
                p[4] = field as u8 - 1;
            }
            if field > 1 {
                p[5] = if first { 0x10 | (key as u8) << 6 } else { 0 };
            }
            if first {
                p[6..12].copy_from_slice(&pcr);
            }
            p[188 - n..].copy_from_slice(&rest[..n]);
            out.extend_from_slice(&p);
            rest = &rest[n..];
            first = false;
        }
        out
    }
}

/// The CRC MPEG-TS tables end with (CRC-32/MPEG-2).
fn mpeg_crc32(data: &[u8]) -> u32 {
    data.iter().fold(u32::MAX, |crc, &b| {
        (0..8).fold(crc ^ (b as u32) << 24, |c, _| if c & 0x8000_0000 != 0 { c << 1 ^ 0x04C1_1DB7 } else { c << 1 })
    })
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

struct RecFrame {
    data: Vec<u8>,
    key: bool,
    us: u32,
}

pub struct Recorder {
    pub path: PathBuf,
    child: Option<Child>,
    tx: Option<std::sync::mpsc::Sender<RecFrame>>,
    queued: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Waiting for a keyframe: at the start, and after dropping frames.
    waiting: bool,
    dropping: bool,
}

impl Recorder {
    /// Record into `dir` as `<base>_<YYYY-MM-DD_HH-MM-SS>.mp4`, starting a new file every `segment_minutes`
    /// (0 = one file). Fragmented MP4 (a fragment per keyframe): a crash loses only the last fragment, and closing
    /// a segment never rewrites it the way faststart did. WebView2 plays and seeks these over the asset protocol.
    /// Timed by the camera's clock, so playback is as steady as the camera however unevenly frames arrived here.
    pub fn new(codec: VideoType, dir: &Path, base: &str, segment_minutes: u32) -> std::io::Result<Self> {
        let template = dir.join(format!("{base}_%Y-%m-%d_%H-%M-%S.mp4")).to_string_lossy().into_owned();
        let seconds = if segment_minutes == 0 { 360_000 } else { segment_minutes * 60 }.to_string();
        let mut child = spawn(
            &[
                "-f", "mpegts", "-i", "pipe:0",
                "-c", "copy", "-f", "segment", "-segment_time", &seconds, "-reset_timestamps", "1",
                "-strftime", "1", "-segment_format", "mp4", "-segment_format_options", "movflags=+frag_keyframe+empty_moov+default_base_moof", &template,
            ],
            false,
        )?;
        let stdin = child.stdin.take().expect("piped stdin");
        let mut mux = Mpegts::new(codec);
        Ok(Self::with_writer(dir.to_owned(), Some(child), stdin, move |f| mux.frame(&f.data, f.key, f.us)))
    }

    /// The writer thread owns all waiting on the disk; the camera loop only ever queues or drops.
    fn with_writer(
        path: PathBuf,
        child: Option<Child>,
        mut out: impl Write + Send + 'static,
        mut encode: impl FnMut(&RecFrame) -> Vec<u8> + Send + 'static,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<RecFrame>();
        let queued = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let q = queued.clone();
        std::thread::spawn(move || {
            for frame in rx {
                if out.write_all(&encode(&frame)).is_err() {
                    break;
                }
                q.fetch_sub(frame.data.len(), std::sync::atomic::Ordering::Relaxed);
            }
        });
        Self { path, child, tx: Some(tx), queued, waiting: true, dropping: false }
    }

    /// Never blocks: a stalled ffmpeg must not stall the camera's media, commands and pings.
    pub fn push(&mut self, data: &[u8], keyframe: bool, us: u32) -> Pushed {
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
        if tx.send(RecFrame { data: data.to_vec(), key: keyframe, us }).is_err() {
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
        assert_eq!(rec.push(b"\0\0\0\x01", true, 0), Pushed::Queued);
        let mut child = rec.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        // The feeder notices on its next write; from then on every push fails.
        let gone = (0..500).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            rec.push(&[0u8; 4096], false, 0) == Pushed::Dead
        });
        assert!(gone, "push kept succeeding after ffmpeg exited");
        drop(rec);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn raw_viewers_start_at_the_last_keyframe() {
        let feed = RawFeed::default();
        let f = |key, n: u8| Raw { hevc: true, key, us: 0, data: vec![n] };
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

    #[test]
    fn mpegts_crc_is_mpeg2() {
        assert_eq!(mpeg_crc32(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn mpegts_time_follows_the_camera_clock() {
        let mut m = Mpegts::new(VideoType::H265);
        let t0 = m.pts(u32::MAX - 33_000);
        assert_eq!(m.pts(u32::MAX) - t0, 2970, "33 ms at 90 kHz");
        assert_eq!(m.pts(32_999) - t0, 5940, "across the clock wrapping");
        assert_eq!(m.pts(32_999) - t0, 8910, "a stuck clock takes the last step");
        assert_eq!(m.pts(1_000) - t0, 11880, "and so does one going backwards");
        assert_eq!(m.pts(5_001_000) - t0, 11880 + 450_000, "a real gap stays a gap");
    }

    #[test]
    fn mpegts_packets_are_well_formed() {
        for len in [0, 1, 150, 161, 162, 163, 176, 500, 100_000] {
            let mut m = Mpegts::new(VideoType::H264);
            let out = m.frame(&vec![0xAB; len], true, 0);
            assert_eq!(out.len() % 188, 0, "{len} bytes");
            let packets: Vec<_> = out.chunks(188).collect();
            assert!(packets.iter().all(|p| p[0] == 0x47));
            let video: Vec<_> = packets.iter().filter(|p| p[1] & 0x1F == 1 && p[2] == 0).collect();
            let mut payload = 0;
            for (i, p) in video.iter().enumerate() {
                assert_eq!(p[3] & 15, i as u8 & 15, "continuity");
                assert_eq!(p[1] & 0x40 != 0, i == 0, "start flag");
                payload += if p[3] & 0x20 != 0 { 183 - p[4] as usize } else { 184 };
            }
            assert_eq!(payload, 14 + len, "PES header and the whole frame, {len} bytes");
        }
    }

    /// Encode `n` frames with an access unit delimiter ahead of each, and split them apart again.
    fn access_units(enc: &str, params: (&str, &str), format: &str, aud: &[u8], n: usize) -> Vec<Vec<u8>> {
        let frames = n.to_string();
        let out = Command::new(ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=15", "-frames:v", &frames, "-c:v", enc, params.0, params.1, "-f", format, "pipe:1"])
            .output()
            .expect("ffmpeg on PATH");
        assert!(out.status.success() && !out.stdout.is_empty());
        let starts: Vec<usize> = out.stdout.windows(aud.len()).enumerate().filter(|(_, w)| *w == aud).map(|(i, _)| i).collect();
        assert_eq!(starts.len(), n, "one delimiter per frame");
        (0..n).map(|i| out.stdout[starts[i]..*starts.get(i + 1).unwrap_or(&out.stdout.len())].to_vec()).collect()
    }

    /// Frames that arrive in bursts with a long stall, stamped at a steady 15 fps, must come out of the file steady.
    fn recording_keeps_camera_time(codec: VideoType, enc: &str, params: (&str, &str), format: &str, aud: &[u8], is_key: fn(&[u8]) -> bool) {
        const N: usize = 45;
        let units = access_units(enc, params, format, aud, N);
        let dir = std::env::temp_dir().join(format!("reolink-rs-rec-time-{enc}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut rec = Recorder::new(codec, &dir, "Cam", 0).unwrap();
        let start = u32::MAX - 1_000_000; // the camera's clock wraps a second in
        for (i, unit) in units.iter().enumerate() {
            if i == 20 {
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
            let us = start.wrapping_add((i as u64 * 1_000_000 / 15) as u32);
            assert_eq!(rec.push(unit, is_key(unit), us), Pushed::Queued);
        }
        let mut child = rec.child.take().unwrap();
        rec.tx.take();
        child.wait().unwrap();

        let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        assert!(std::fs::read(&file).unwrap().windows(4).any(|w| w == b"moof"), "fragmented");
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
            .arg(&file)
            .output()
            .expect("ffprobe on PATH");
        assert!(probe.status.success());
        let text = String::from_utf8(probe.stdout).unwrap();
        let rows: Vec<(f64, bool)> = text.lines().filter(|l| !l.is_empty()).map(|l| {
            let (t, flags) = l.split_once(',').unwrap();
            (t.parse().unwrap(), flags.starts_with('K'))
        }).collect();
        assert_eq!(rows.len(), N, "every frame in the file");
        assert!(rows[0].1, "starts on a keyframe");
        assert!(rows[0].0.abs() < 0.001, "starts at zero, got {}", rows[0].0);
        for w in rows.windows(2) {
            let gap = w[1].0 - w[0].0;
            assert!((gap - 1.0 / 15.0).abs() < 0.002, "frame gap {gap}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn h264_recording_keeps_camera_time() {
        recording_keeps_camera_time(VideoType::H264, "libx264", ("-x264-params", "aud=1:keyint=15:bframes=0"), "h264", &[0, 0, 0, 1, 9], |u| u.windows(4).any(|w| w == [0, 0, 1, 0x65]));
    }

    #[test]
    fn h265_recording_keeps_camera_time() {
        recording_keeps_camera_time(VideoType::H265, "libx265", ("-x265-params", "aud=1:keyint=15:bframes=0:log-level=error"), "hevc", &[0, 0, 0, 1, 0x46, 1], |u| u.windows(4).any(|w| w[..3] == [0, 0, 1] && matches!((w[3] >> 1) & 0x3F, 16..=21)));
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
        let mut r = Recorder::with_writer(PathBuf::new(), None, Gate(gate, got.clone()), |f| f.data.clone());
        let mb = vec![1u8; 1 << 20];
        assert_eq!(r.push(&[9], false, 0), Pushed::Queued); // before the first keyframe: skipped
        // A stalled disk: the queue fills, then frames drop with one report, all without blocking.
        let t = std::time::Instant::now();
        let results: Vec<_> = (0..80).map(|i| r.push(&mb, i == 0, 0)).collect();
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
        assert_eq!(r.push(&[2], false, 0), Pushed::Queued);
        assert_eq!(r.push(&[3], true, 0), Pushed::Queued);
        open.send(()).unwrap();
        while r.queued.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            std::thread::yield_now();
        }
        let got = got.lock().unwrap();
        assert_eq!(got.len(), REC_QUEUE_BYTES + 1);
        assert_eq!(got.last(), Some(&3));
    }
}
