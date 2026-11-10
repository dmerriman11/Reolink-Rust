//! Camera SD playback: search through the existing encrypted camera session, cache on demand.
//!
//! A download plays while it runs: the frames are piped through ffmpeg into fragmented MP4 files (one per track)
//! that the page feeds to a MediaSource as they grow, while the finished clip is muxed once more into a plain MP4
//! for the cache. See [`Progress`] and ui/sdfeed.js.
use chrono::{Datelike, Timelike, NaiveDate, NaiveDateTime};
use neolink_core::bc_protocol::{BcCamera, SdChunk};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub start: String,
    pub end: String,
    /// The camera's `recordType`/`alarmType` for the file, e.g. "people,vehicle"; empty when it didn't say.
    #[serde(default)]
    pub types: String,
}
#[derive(Deserialize)]
struct OpenReply { #[serde(rename="findAlarmVideo", alias="alarmVideoInfo")] info: Handle }
#[derive(Deserialize)]
struct Handle { #[serde(rename="fileHandle")] handle: String }
#[derive(Deserialize)]
struct Page { #[serde(rename="alarmVideoInfo")] info: Info }
#[derive(Deserialize)]
struct Info { #[serde(rename="bFinished")] finished: u8, #[serde(rename="alarmVideoList", default)] list: Items }
#[derive(Deserialize, Default)]
struct Items { #[serde(rename="alarmVideo", default)] clips: Vec<Item> }
#[derive(Deserialize)]
struct Item { #[serde(rename="fileName")] name: String, #[serde(rename="startTime")] start: Clock, #[serde(rename="endTime")] end: Clock, #[serde(rename="alarmType", default)] types: String }
#[derive(Deserialize)]
struct Clock { year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32 }
impl Clock {
    fn date(&self) -> Result<NaiveDateTime, String> {
        NaiveDate::from_ymd_opt(self.year,self.month,self.day).and_then(|d|d.and_hms_opt(self.hour,self.minute,self.second)).ok_or("Invalid camera recording time".into())
    }
}
fn time_xml(t: NaiveDateTime) -> String { format!("<year>{}</year><month>{}</month><day>{}</day><hour>{}</hour><minute>{}</minute><second>{}</second>",t.year(),t.month(),t.day(),t.hour(),t.minute(),t.second()) }
fn parse_time(s: &str) -> Result<NaiveDateTime, String> { NaiveDateTime::parse_from_str(s,"%Y-%m-%dT%H:%M:%S").map_err(|_|"Invalid recording time".into()) }
/// The end of a clip's first few seconds: enough for a thumbnail without fetching the whole clip.
pub fn head_end(start: &str, end: &str) -> Result<String, String> {
    let (a, b) = (parse_time(start)?, parse_time(end)?);
    Ok(b.min(a + chrono::Duration::seconds(3)).format("%Y-%m-%dT%H:%M:%S").to_string())
}
fn clip(name: String, a: NaiveDateTime, b: NaiveDateTime, types: String) -> Clip {
    Clip { name, start: a.format("%Y-%m-%dT%H:%M:%S").to_string(), end: b.format("%Y-%m-%dT%H:%M:%S").to_string(), types }
}
async fn request(cam: &BcCamera, id:u32, xml:String) -> Result<String,String> {
    tokio::time::timeout(std::time::Duration::from_secs(20),cam.request_xml(id,xml)).await.map_err(|_|"Camera SD request timed out".to_string())?.map_err(|e|format!("Camera SD: {e}"))
}
pub async fn search(cam:&BcCamera, channel:u8, day:&str) -> Result<Vec<Clip>,String> {
    NaiveDate::parse_from_str(day,"%Y-%m-%d").map_err(|_|"Invalid day")?;
    let uid=tokio::time::timeout(std::time::Duration::from_secs(20),cam.uid()).await.map_err(|_|"Camera UID request timed out")?.map_err(|e|e.to_string())?;
    let uid=quick_xml::escape::escape(&uid);
    match search_files(cam,channel,day,&uid).await {
        Ok(clips)=>Ok(clips),
        Err(e) if e.contains("405") || e.contains("400") => search_alarm(cam,channel,day,&uid).await,
        Err(e)=>Err(e)
    }
}
async fn search_alarm(cam:&BcCamera, channel:u8, day:&str, uid:&str) -> Result<Vec<Clip>,String> {
    let date=NaiveDate::parse_from_str(day,"%Y-%m-%d").map_err(|_|"Invalid day")?;
    let mut start=date.and_hms_opt(0,0,0).unwrap();
    let end=date.and_hms_opt(23,59,59).unwrap();
    let mut clips=std::collections::BTreeMap::new();
    for _ in 0..100 {
        let xml=format!("<body><findAlarmVideo version=\"1.1\"><channelId>{channel}</channelId><uid>{uid}</uid><logicChnBitmap>255</logicChnBitmap><streamType>0</streamType><notSearchVideo>0</notSearchVideo><startTime>{}</startTime><endTime>{}</endTime><alarmType>md,pir,io,people,face,vehicle,dog_cat,visitor,other,package,cry,crossline,intrusion,loitering,legacy,loss</alarmType></findAlarmVideo></body>",time_xml(start),time_xml(end));
        let open=request(cam,272,xml).await?;
        let handle:OpenReply=quick_xml::de::from_str(&open).map_err(|e|format!("Invalid SD search reply: {e}"))?;
        let xml=format!("<body><findAlarmVideo version=\"1.1\"><channelId>{channel}</channelId><fileHandle>{}</fileHandle></findAlarmVideo></body>",quick_xml::escape::escape(&handle.info.handle));
        let page=request(cam,273,xml.clone()).await;
        let close=request(cam,274,xml).await;
        let text=page?;
        // Some cameras acknowledge close without XML.
        let _ = close;
        let page:Page=quick_xml::de::from_str(&text).map_err(|e|format!("Invalid SD clip list: {e}"))?;
        let mut last=start;
        for c in page.info.list.clips {
            let a=c.start.date()?;let b=c.end.date()?;
            if b<=a {continue;}
            last=last.max(a);
            clips.insert((a,c.name.clone()),clip(c.name,a,b,c.types));
        }
        if page.info.finished!=0 {return Ok(clips.into_values().collect());}
        if last<=start {return Err("Camera SD search did not advance; results may be incomplete".into());}
        start=last;
    }
    Err("Camera SD search exceeded its page limit".into())
}
#[derive(Deserialize)]
struct FilePage { #[serde(rename="FileInfoList")] list: FileItems }
#[derive(Deserialize)]
struct FileItems { #[serde(rename="FileInfo",default)] clips: Vec<FileItem> }
#[derive(Deserialize)]
struct FileItem { #[serde(rename="name",alias="fileName")] name:String, #[serde(rename="startTime")] start:Clock, #[serde(rename="endTime")] end:Clock, #[serde(rename="recordType",default)] types:String }
async fn search_files(cam:&BcCamera,channel:u8,day:&str,uid:&str)->Result<Vec<Clip>,String> {
    let date=NaiveDate::parse_from_str(day,"%Y-%m-%d").map_err(|_|"Invalid day")?;
    let xml=format!("<body><FileInfoList version=\"1.1\"><FileInfo><uid>{uid}</uid><searchAITrack>1</searchAITrack><channelId>{channel}</channelId><logicChnBitmap>255</logicChnBitmap><streamType>mainStream</streamType><recordType>manual,sched,io,md,people,face,vehicle,dog_cat,visitor,other,package,crossline,intrusion,loitering,legacy,loss</recordType><startTime>{}</startTime><endTime>{}</endTime></FileInfo></FileInfoList></body>",time_xml(date.and_hms_opt(0,0,0).unwrap()),time_xml(date.and_hms_opt(23,59,59).unwrap()));
    let open=request(cam,14,xml).await?;
    let handle=crate::xmledit::get(&open,"FileInfoList/FileInfo/handle").ok_or_else(||"SD search did not return a file handle".to_string())?;
    let xml=format!("<body><FileInfoList version=\"1.1\"><FileInfo><channelId>{channel}</channelId><uid>{uid}</uid><searchAITrack>1</searchAITrack><handle>{}</handle></FileInfo></FileInfoList></body>",quick_xml::escape::escape(&handle));
    let result=async {
        let mut clips=std::collections::BTreeMap::new();
        for _ in 0..100 {
            let text=request(cam,15,xml.clone()).await?;
            let page:FilePage=quick_xml::de::from_str(&text).map_err(|e|format!("Invalid SD file list: {e}"))?;
            let n=page.list.clips.len();
            for c in page.list.clips {let a=c.start.date()?;let b=c.end.date()?;if b>a {clips.insert((a,c.name.clone()),clip(c.name,a,b,c.types));}}
            let done=crate::xmledit::get(&text,"FileInfoList/bFinished").or_else(||crate::xmledit::get(&text,"FileInfoList/finished"));
            if done.as_deref()==Some("1") || (done.is_none() && n<40) {return Ok(clips.into_values().collect());}
        }
        Err("SD file search exceeded its page limit".into())
    }.await;
    let _=request(cam,16,xml).await;
    result
}

/// One growing fragmented-MP4 file of a download in progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track { pub path: PathBuf, pub mime: String, pub bytes: u64 }

/// What the page polls while a clip downloads. `tracks` grow until `done`; then either `file` (the cached MP4)
/// or `error` is set. Tracks are empty when progressive playback isn't possible (no ffmpeg, unknown codec).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub key: String,
    pub tracks: Vec<Track>,
    /// Seconds of video received so far (frames / the stream's stated frame rate), for a progress bar.
    pub seconds: f64,
    pub done: bool,
    pub file: Option<PathBuf>,
    pub error: Option<String>,
    #[serde(skip)]
    pub abort: Option<tokio::task::AbortHandle>,
}
pub type Shared = Arc<Mutex<Progress>>;

/// A snapshot with the tracks' current sizes.
pub fn snapshot(shared: &Shared) -> Progress {
    let mut p = shared.lock().unwrap().clone();
    for t in &mut p.tracks { t.bytes = std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0); }
    p
}

/// The cache file a clip downloads to; the progress key is its path.
pub fn dest(cache: &Path, start: &str, end: &str) -> Result<PathBuf, String> {
    let a=parse_time(start)?;let b=parse_time(end)?;
    if b<=a || b-a>chrono::Duration::days(1) {return Err("Invalid SD clip range".into());}
    Ok(cache.join(format!("{}_{}.mp4",a.format("%Y%m%dT%H%M%S"),b.format("%Y%m%dT%H%M%S"))))
}

/// ffmpeg writing a fragmented MP4 from frames fed to its stdin.
struct Pipe { child: tokio::process::Child, stdin: tokio::process::ChildStdin }
impl Pipe {
    fn spawn(input: &[&str], output: &[&str], out: &Path) -> Result<Pipe, String> {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new(crate::media::ffmpeg());
        cmd.args(["-hide_banner", "-loglevel", "error"]).args(input).args(["-i", "pipe:0", "-c", "copy"]).args(output)
            .args(["-movflags", "empty_moov+default_base_moof", "-flush_packets", "1", "-f", "mp4", "-y"]).arg(out)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
        let stdin = child.stdin.take().ok_or("ffmpeg stdin")?;
        Ok(Pipe { child, stdin })
    }
    async fn write(&mut self, data: &[u8]) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        self.stdin.write_all(data).await.map_err(|e| format!("ffmpeg stopped: {e}"))
    }
    async fn finish(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), self.child.wait()).await;
    }
}

/// The MediaSource codec string for the first access unit of a stream, from its parameter sets.
fn codec_string(codec: &str, au: &[u8]) -> Option<String> {
    // The wanted NAL unit's first 24 bytes with emulation-prevention bytes (00 00 03) removed.
    let nal = |want: &dyn Fn(u8) -> bool| -> Option<Vec<u8>> {
        let mut i = 0;
        while i + 3 < au.len() {
            let n = if au[i..].starts_with(&[0,0,0,1]) { i + 4 } else if au[i..].starts_with(&[0,0,1]) { i + 3 } else { i += 1; continue };
            if au.get(n).is_some_and(|&b| want(b)) {
                let mut out = vec![];
                for (k, &b) in au[n..].iter().enumerate() {
                    // emulation prevention: a 03 after two zero bytes on the wire (not in the output)
                    if b == 3 && k >= 2 && au[n + k - 2..n + k] == [0, 0] { continue; }
                    out.push(b);
                    if out.len() == 24 { break; }
                }
                return Some(out);
            }
            i = n;
        }
        None
    };
    if codec == "h264" {
        let sps = nal(&|b| b & 31 == 7)?;
        return Some(format!("avc1.{:02X}{:02X}{:02X}", sps.get(1)?, sps.get(2)?, sps.get(3)?));
    }
    // HEVC SPS: 2-byte header, 1 byte of ids, then profile_tier_level.
    let sps = nal(&|b| (b >> 1) & 63 == 33)?;
    let ptl = sps.get(3..15)?;
    let profile = ptl[0] & 31;
    let tier = if ptl[0] & 32 != 0 { 'H' } else { 'L' };
    let compat = u32::from_be_bytes([ptl[1], ptl[2], ptl[3], ptl[4]]).reverse_bits();
    let mut constraints: Vec<String> = ptl[5..11].iter().map(|b| format!("{b:02X}")).collect();
    while constraints.last().is_some_and(|c| c == "00") { constraints.pop(); }
    let tail = if constraints.is_empty() { String::new() } else { format!(".{}", constraints.join(".")) };
    Some(format!("hvc1.{profile}.{compat:X}.{tier}{}{tail}", ptl[11]))
}

/// "mp4a.40.<object type>" from an ADTS frame header.
fn aac_codec(adts: &[u8]) -> String { format!("mp4a.40.{}", adts.get(2).map_or(2, |b| (b >> 6) + 1)) }

/// Files a download leaves behind; the camera is told to stop too, so an aborted task doesn't leave it sending.
struct Cleanup { cam: Arc<BcCamera>, channel: u8, remove: Vec<PathBuf> }
impl Drop for Cleanup {
    fn drop(&mut self) {
        for f in &self.remove { let _ = std::fs::remove_file(f); }
        let (cam, channel) = (self.cam.clone(), self.channel);
        let stop=format!("<body><FileInfoList version=\"1.1\"><FileInfo><channelId>{channel}</channelId></FileInfo></FileInfoList></body>");
        tokio::spawn(async move { let _=tokio::time::timeout(std::time::Duration::from_secs(3),cam.set_xml(144,false,stop)).await; });
    }
}

/// Download one clip into `dest` (see [`dest`]), publishing growing fragment files in `progress` on the way.
pub async fn download(cam: Arc<BcCamera>, channel:u8, start:&str, end:&str, dest: PathBuf, progress: &Shared) -> Result<PathBuf,String> {
    let a=parse_time(start)?;let b=parse_time(end)?;
    let cache = dest.parent().ok_or("cache dir")?;
    std::fs::create_dir_all(cache).map_err(|e|e.to_string())?;
    if dest.is_file() {return Ok(dest);}
    // Leftovers of earlier downloads (finished ones keep their fragments until now, for the page still reading them).
    for e in std::fs::read_dir(cache).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().contains(".partial.") { let _ = std::fs::remove_file(e.path()); }
    }
    let stem = dest.with_extension("");
    let raw=stem.with_extension("video");
    let audio=stem.with_extension("aac");
    let temp=stem.with_extension("partial.mp4");
    let frag_v=stem.with_extension("partial.video.mp4");
    let frag_a=stem.with_extension("partial.audio.mp4");
    let mut cleanup = Cleanup { cam: cam.clone(), channel, remove: vec![raw.clone(), audio.clone(), temp.clone(), frag_v.clone(), frag_a.clone()] };
    let xml=format!("<body><FileInfoList version=\"1.1\"><FileInfo><logicChnBitmap>255</logicChnBitmap><channelId>{channel}</channelId><supportSub>0</supportSub><streamType>mainStream</streamType><startTime>{}</startTime><endTime>{}</endTime></FileInfo></FileInfoList></body>",time_xml(a),time_xml(b));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut fetch = { let cam = cam.clone(); tokio::spawn(async move { cam.download_recording(xml, tx).await }) };
    let result: Result<(), String> = async {
        use tokio::io::AsyncWriteExt as _;
        let mut video_file = tokio::fs::File::create(&raw).await.map_err(|e| e.to_string())?;
        let mut audio_file = tokio::fs::File::create(&audio).await.map_err(|e| e.to_string())?;
        let (mut codec, mut fps) = (String::new(), 0u8);
        let (mut pipe_v, mut pipe_a): (Option<Pipe>, Option<Pipe>) = (None, None);
        let mut first_video = true;
        let mut frames = 0u64;
        let publish = |path: &Path, mime: String| progress.lock().unwrap().tracks.push(Track { path: path.to_owned(), mime, bytes: 0 });
        while let Some(chunk) = rx.recv().await {
            match chunk {
                SdChunk::Codec { codec: c, fps: f } => { codec = c; fps = f; }
                SdChunk::Video(data) => {
                    video_file.write_all(&data).await.map_err(|e| e.to_string())?;
                    frames += 1; // one chunk is one frame
                    progress.lock().unwrap().seconds = frames as f64 / f64::from(fps.max(1));
                    if first_video {
                        first_video = false;
                        // No parameter sets: play after the download instead.
                        if let Some(cs) = codec_string(&codec, &data) {
                            // A fragment per keyframe (every 2 s on Reolink cameras) is what the page can start on.
                            match Pipe::spawn(&["-f", &codec, "-r", &fps.to_string()], &["-tag:v", if codec == "hevc" { "hvc1" } else { "avc1" }, "-frag_duration", "1"], &frag_v) {
                                Ok(p) => { pipe_v = Some(p); publish(&frag_v, format!("video/mp4; codecs=\"{cs}\"")); }
                                Err(e) => log::warn!("SD progressive video: {e}"),
                            }
                        }
                    }
                    if let Some(p) = pipe_v.as_mut() && let Err(e) = p.write(&data).await { log::warn!("SD progressive video: {e}"); pipe_v = None; }
                }
                SdChunk::Audio(data) => {
                    audio_file.write_all(&data).await.map_err(|e| e.to_string())?;
                    if pipe_a.is_none() && pipe_v.is_some() && !progress.lock().unwrap().tracks.iter().any(|t| t.path == frag_a) {
                        // ADTS headers must come off for MP4 (ffmpeg only inserts the filter by itself for file inputs).
                        match Pipe::spawn(&["-f", "aac"], &["-bsf:a", "aac_adtstoasc", "-frag_duration", "1000000"], &frag_a) {
                            Ok(p) => { pipe_a = Some(p); publish(&frag_a, format!("audio/mp4; codecs=\"{}\"", aac_codec(&data))); }
                            Err(e) => log::warn!("SD progressive audio: {e}"),
                        }
                    }
                    if let Some(p) = pipe_a.as_mut() && let Err(e) = p.write(&data).await { log::warn!("SD progressive audio: {e}"); pipe_a = None; }
                }
            }
        }
        video_file.flush().await.map_err(|e| e.to_string())?;
        audio_file.flush().await.map_err(|e| e.to_string())?;
        if let Some(p) = pipe_v { p.finish().await; }
        if let Some(p) = pipe_a { p.finish().await; }
        let (codec, fps) = (&mut fetch).await.map_err(|_| "SD download task failed".to_string())?.map_err(|e| e.to_string())?;
        let fps=fps.to_string();let o=std::ffi::OsStr::new;
        let mut args=vec![o("-y"),o("-r"),o(&fps),o("-f"),o(&codec),o("-i"),raw.as_os_str()];
        if std::fs::metadata(&audio).is_ok_and(|m|m.len()>0) {args.extend([o("-f"),o("aac"),o("-i"),audio.as_os_str()]);}
        args.extend([o("-c"),o("copy"),o("-tag:v"),o(if codec=="hevc" {"hvc1"} else {"avc1"}),o("-movflags"),o("+faststart"),temp.as_os_str()]);
        crate::media::run(&args).await?;
        std::fs::rename(&temp,&dest).map_err(|e|e.to_string())?;
        Ok(())
    }.await;
    fetch.abort();
    if result.is_ok() { cleanup.remove.retain(|f| f != &frag_v && f != &frag_a); }
    drop(cleanup);
    result.map(|_| dest)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn thumbnail_heads_stay_inside_the_clip() {
        assert_eq!(head_end("2026-11-09T10:00:00","2026-11-09T10:05:00").unwrap(),"2026-11-09T10:00:03");
        assert_eq!(head_end("2026-11-09T10:00:00","2026-11-09T10:00:02").unwrap(),"2026-11-09T10:00:02");
    }
    #[test] fn camera_clock_and_ranges() {
        assert_eq!(time_xml(parse_time("2026-10-30T12:34:56").unwrap()),"<year>2026</year><month>10</month><day>30</day><hour>12</hour><minute>34</minute><second>56</second>");
        assert!(parse_time("../bad").is_err());
        assert!(Clock{year:2026,month:2,day:30,hour:0,minute:0,second:0}.date().is_err());
        let p:Page=quick_xml::de::from_str("<body><alarmVideoInfo><bFinished>1</bFinished><alarmVideoList/></alarmVideoInfo></body>").unwrap();assert!(p.info.list.clips.is_empty());
        assert!(dest(Path::new("c"), "2026-10-30T12:00:00", "2026-10-30T11:00:00").is_err());
        assert_eq!(dest(Path::new("c"), "2026-10-30T12:00:00", "2026-10-30T12:01:00").unwrap(), Path::new("c").join("20261030T120000_20261030T120100.mp4"));
    }
    #[test] fn file_list_carries_record_types() {
        let xml = "<body><FileInfoList version=\"1.1\"><FileInfo><channelId>0</channelId><name>0120261107115307</name><recordType>people,vehicle,other</recordType><startTime><year>2026</year><month>11</month><day>7</day><hour>11</hour><minute>53</minute><second>7</second></startTime><endTime><year>2026</year><month>11</month><day>7</day><hour>11</hour><minute>54</minute><second>55</second></endTime></FileInfo></FileInfoList></body>";
        let page: FilePage = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(page.list.clips[0].types, "people,vehicle,other");
        let c = clip("n".into(), page.list.clips[0].start.date().unwrap(), page.list.clips[0].end.date().unwrap(), page.list.clips[0].types.clone());
        assert_eq!((c.start.as_str(), c.end.as_str()), ("2026-11-07T11:53:07", "2026-11-07T11:54:55"));
    }
    #[test] fn media_source_codec_strings_come_from_the_parameter_sets() {
        // H.264 High 4.0: SPS 67 64 00 28.
        assert_eq!(codec_string("h264", &[0,0,0,1,0x67,0x64,0x00,0x28,0xAC]).as_deref(), Some("avc1.640028"));
        // HEVC Main, tier L, level 5.0 (150), compat flags 0110 0000..., constraints 90 00 00 00 00 00; the zero
        // runs carry emulation-prevention bytes on the wire (as the Argus 4 Pro sends them).
        let sps = [0,0,0,1,0x42,0x01, 0x01, 0x01,0x60,0,0,3,0, 0x90,0,0,3,0,0,3,0, 150, 0xFF];
        assert_eq!(codec_string("hevc", &sps).as_deref(), Some("hvc1.1.6.L150.90"));
        let plain = [0,0,0,1,0x42,0x01, 0x01, 0x01,0x60,0,0,3,0, 0,0,3,0,0,3,0,0, 150, 0xFF];
        assert_eq!(codec_string("hevc", &plain).as_deref(), Some("hvc1.1.6.L150"), "all-zero constraint flags are left out, as the Argus 4 Pro sends them");
        assert_eq!(codec_string("hevc", &[0,0,0,1,0x26,0x01,0xAF]), None);
        assert_eq!(aac_codec(&[0xFF,0xF1,0x4C]), "mp4a.40.2");
    }
}
