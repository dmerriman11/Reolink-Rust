//! Camera SD playback: search through the existing encrypted camera session, cache on demand.
use chrono::{Datelike, Timelike, NaiveDate, NaiveDateTime};
use neolink_core::bc_protocol::BcCamera;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip { pub name: String, pub start: String, pub end: String }
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
struct Item { #[serde(rename="fileName")] name: String, #[serde(rename="startTime")] start: Clock, #[serde(rename="endTime")] end: Clock }
#[derive(Deserialize)]
struct Clock { year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32 }
impl Clock {
    fn date(&self) -> Result<NaiveDateTime, String> {
        NaiveDate::from_ymd_opt(self.year,self.month,self.day).and_then(|d|d.and_hms_opt(self.hour,self.minute,self.second)).ok_or("Invalid camera recording time".into())
    }
}
fn time_xml(t: NaiveDateTime) -> String { format!("<year>{}</year><month>{}</month><day>{}</day><hour>{}</hour><minute>{}</minute><second>{}</second>",t.year(),t.month(),t.day(),t.hour(),t.minute(),t.second()) }
fn parse_time(s: &str) -> Result<NaiveDateTime, String> { NaiveDateTime::parse_from_str(s,"%Y-%m-%dT%H:%M:%S").map_err(|_|"Invalid recording time".into()) }
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
            clips.insert((a,c.name.clone()),Clip{name:c.name,start:a.format("%Y-%m-%dT%H:%M:%S").to_string(),end:b.format("%Y-%m-%dT%H:%M:%S").to_string()});
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
struct FileItem { #[serde(rename="name",alias="fileName")] name:String, #[serde(rename="startTime")] start:Clock, #[serde(rename="endTime")] end:Clock }
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
            for c in page.list.clips {let a=c.start.date()?;let b=c.end.date()?;if b>a {clips.insert((a,c.name.clone()),Clip{name:c.name,start:a.format("%Y-%m-%dT%H:%M:%S").to_string(),end:b.format("%Y-%m-%dT%H:%M:%S").to_string()});}}
            let done=crate::xmledit::get(&text,"FileInfoList/bFinished").or_else(||crate::xmledit::get(&text,"FileInfoList/finished"));
            if done.as_deref()==Some("1") || (done.is_none() && n<40) {return Ok(clips.into_values().collect());}
        }
        Err("SD file search exceeded its page limit".into())
    }.await;
    let _=request(cam,16,xml).await;
    result
}
pub async fn download(cam:&BcCamera, channel:u8, start:&str,end:&str, cache:&Path) -> Result<PathBuf,String> {
    let a=parse_time(start)?;let b=parse_time(end)?;
    if b<=a || b-a>chrono::Duration::days(1) {return Err("Invalid SD clip range".into());}
    std::fs::create_dir_all(cache).map_err(|e|e.to_string())?;
    let stem=format!("{}_{}",a.format("%Y%m%dT%H%M%S"),b.format("%Y%m%dT%H%M%S"));
    let dest=cache.join(format!("{stem}.mp4"));
    if dest.is_file() {return Ok(dest);}
    let raw=cache.join(format!("{stem}.video"));
    let audio=raw.with_extension("aac");
    let temp=cache.join(format!("{stem}.partial.mp4"));
    let xml=format!("<body><FileInfoList version=\"1.1\"><FileInfo><logicChnBitmap>255</logicChnBitmap><channelId>{channel}</channelId><supportSub>0</supportSub><streamType>mainStream</streamType><startTime>{}</startTime><endTime>{}</endTime></FileInfo></FileInfoList></body>",time_xml(a),time_xml(b));
    let result=async {
        let (codec,fps)=tokio::time::timeout(std::time::Duration::from_secs(300),cam.download_recording(xml,&raw)).await.map_err(|_|"SD download exceeded five minutes".to_string())?.map_err(|e|e.to_string())?;
        let fps=fps.to_string();let o=std::ffi::OsStr::new;
        let mut args=vec![o("-y"),o("-r"),o(&fps),o("-f"),o(&codec),o("-i"),raw.as_os_str()];
        if std::fs::metadata(&audio).is_ok_and(|m|m.len()>0) {args.extend([o("-f"),o("aac"),o("-i"),audio.as_os_str()]);}
        args.extend([o("-c"),o("copy"),o("-tag:v"),o(if codec=="hevc" {"hvc1"} else {"avc1"}),o("-movflags"),o("+faststart"),temp.as_os_str()]);
        crate::media::run(&args).await?;
        std::fs::rename(&temp,&dest).map_err(|e|e.to_string())?;
        Ok(dest)
    }.await;
    let stop=format!("<body><FileInfoList version=\"1.1\"><FileInfo><channelId>{channel}</channelId></FileInfo></FileInfoList></body>");
    let _=tokio::time::timeout(std::time::Duration::from_secs(3),cam.set_xml(144,false,stop)).await;
    let _=std::fs::remove_file(audio);let _=std::fs::remove_file(raw);let _=std::fs::remove_file(temp);
    result
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn camera_clock_and_ranges() {
        assert_eq!(time_xml(parse_time("2026-10-30T12:34:56").unwrap()),"<year>2026</year><month>10</month><day>30</day><hour>12</hour><minute>34</minute><second>56</second>");
        assert!(parse_time("../bad").is_err());
        assert!(Clock{year:2026,month:2,day:30,hour:0,minute:0,second:0}.date().is_err());
        let p:Page=quick_xml::de::from_str("<body><alarmVideoInfo><bFinished>1</bFinished><alarmVideoList/></alarmVideoInfo></body>").unwrap();assert!(p.info.list.clips.is_empty());
    }
}
