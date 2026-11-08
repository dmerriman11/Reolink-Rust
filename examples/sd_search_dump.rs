//! Read-only: list one day's SD recordings on each saved camera and print the raw file-list XML, so the
//! per-file fields (record/alarm types) can be read: `cargo run --example sd_search_dump -- [YYYY-MM-DD]`.
use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let day = std::env::args().nth(1).unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let d = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")?;
    let t = |h: u32, m: u32, s: u32| format!("<year>{}</year><month>{}</month><day>{}</day><hour>{h}</hour><minute>{m}</minute><second>{s}</second>", d.format("%Y"), d.format("%-m"), d.format("%-d"));
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    for dev in saved["devices"].as_array().unwrap() {
        let s = |k: &str| dev[k].as_str().unwrap_or("").to_string();
        let cam = BcCamera::new(&BcCameraOpt {
            name: s("name"), channel_id: 0, addrs: s("host").parse().ok().into_iter().collect(),
            uid: Some(s("uid")).filter(|u| !u.is_empty()), port: None, protocol: ConnectionProtocol::TcpUdp,
            discovery: DiscoveryMethods::Local, max_discovery_retries: 5,
            credentials: Credentials { username: s("username"), password: Some(password(&s("secret"), s("password"))) }, debug: false,
        }).await?;
        cam.login().await?;
        let uid = cam.uid().await?;
        let open = format!("<body><FileInfoList version=\"1.1\"><FileInfo><uid>{uid}</uid><searchAITrack>1</searchAITrack><channelId>0</channelId><logicChnBitmap>255</logicChnBitmap><streamType>mainStream</streamType><recordType>manual,sched,io,md,people,face,vehicle,dog_cat,visitor,other,package,crossline,intrusion,loitering,legacy,loss</recordType><startTime>{}</startTime><endTime>{}</endTime></FileInfo></FileInfoList></body>", t(0,0,0), t(23,59,59));
        let reply = cam.request_xml(14, open).await?;
        println!("==== {} open\n{reply}", s("name"));
        if let Some(h) = handle(&reply) {
            let page = format!("<body><FileInfoList version=\"1.1\"><FileInfo><channelId>0</channelId><uid>{uid}</uid><searchAITrack>1</searchAITrack><handle>{h}</handle></FileInfo></FileInfoList></body>");
            let text = cam.request_xml(15, page.clone()).await?;
            println!("==== {} page (first 6000 chars)\n{}", s("name"), &text[..text.len().min(6000)]);
            let _ = cam.request_xml(16, page).await;
        }
        let _ = cam.logout().await;
    }
    Ok(())
}

fn handle(xml: &str) -> Option<&str> {
    let i = xml.find("<handle>")? + 8;
    Some(&xml[i..i + xml[i..].find("</handle>")?])
}

fn password(secret: &str, in_file: String) -> String {
    keyring::Entry::new("Reolink-rs", secret).and_then(|e| e.get_password()).unwrap_or(in_file)
}
