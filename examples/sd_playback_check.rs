#![allow(dead_code)]
//! Read-only SD playback check: cargo run --example sd_playback_check -- [camera name] [YYYY-MM-DD] [--download]
#[path="../src/xmledit.rs"] mod xmledit;
#[path="../src/sd.rs"] mod sd;
#[path="../src/media.rs"] mod media;
#[path="../src/prefs.rs"] mod prefs;
use neolink_core::bc_protocol::{BcCamera,BcCameraOpt,ConnectionProtocol,Credentials,DiscoveryMethods};
#[tokio::main] async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args:Vec<_>=std::env::args().collect();
    let saved:serde_json::Value=serde_json::from_slice(&std::fs::read(std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json"))?)?;
    let d=saved["devices"].as_array().unwrap().iter().find(|d|args.get(1).is_none_or(|n|d["name"].as_str()==Some(n))).ok_or_else(||anyhow::anyhow!("Camera not found"))?;
    let s=|k:&str|d[k].as_str().unwrap_or("").to_string();
    let password=if s("password").is_empty(){keyring::Entry::new("Reolink-rs",&s("secret"))?.get_password()?}else{s("password")};
    let cam=BcCamera::new(&BcCameraOpt{name:s("name"),channel_id:d["channel"].as_u64().unwrap_or(0) as u8,addrs:s("host").parse().ok().into_iter().collect(),uid:Some(s("uid")).filter(|x|!x.is_empty()),port:d["port"].as_u64().map(|p|p as u16),protocol:ConnectionProtocol::TcpUdp,discovery:DiscoveryMethods::Local,max_discovery_retries:3,credentials:Credentials{username:s("username"),password:Some(password)},debug:false}).await?;
    cam.login().await?;
    if d["aes"].as_bool()==Some(true) && !cam.is_aes(){anyhow::bail!("Camera encryption downgraded");}
    let day=args.get(2).cloned().unwrap_or_else(||chrono::Local::now().format("%Y-%m-%d").to_string());
    let clips=sd::search(&cam,d["channel"].as_u64().unwrap_or(0) as u8,&day).await.map_err(anyhow::Error::msg)?;
    println!("{}: {} SD clips on {}",s("name"),clips.len(),day);
    if args.iter().any(|s|s=="--download") {if let Some(c)=clips.iter().min_by_key(|c| { let parse=|s:&str|chrono::NaiveDateTime::parse_from_str(s,"%Y-%m-%dT%H:%M:%S").unwrap(); parse(&c.end)-parse(&c.start) }){let sample_end=if args.iter().any(|a|a=="--sample") {(chrono::NaiveDateTime::parse_from_str(&c.start,"%Y-%m-%dT%H:%M:%S").unwrap()+chrono::Duration::seconds(5)).format("%Y-%m-%dT%H:%M:%S").to_string()}else{c.end.clone()};let dest=sd::dest(&std::env::temp_dir().join("reolink-sd-check"),&c.start,&sample_end).map_err(anyhow::Error::msg)?;let progress=std::sync::Arc::new(std::sync::Mutex::new(sd::Progress{key:dest.to_string_lossy().into_owned(),..Default::default()}));let path=sd::download(std::sync::Arc::new(cam),d["channel"].as_u64().unwrap_or(0) as u8,&c.start,&sample_end,dest,&progress).await.map_err(anyhow::Error::msg)?;println!("Progressive tracks: {:?}",sd::snapshot(&progress).tracks);println!("Downloaded playable cache: {} ({} bytes)",path.display(),std::fs::metadata(&path)?.len());}}
    Ok(())
}
