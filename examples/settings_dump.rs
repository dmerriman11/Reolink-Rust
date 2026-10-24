//! Read every safe (read-only) settings message from each saved camera and save the XML for inspection:
//! `cargo run --example settings_dump -- <out dir>`. Passwords are redacted before writing.
use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods};

/// Read-only message ids (from the Baichuan dissector table); never writes, reboot, restore, format or upgrade.
const GETS: &[(u32, &str)] = &[
    (26, "VideoInput"), (37, "ServerPort"), (38, "Ntp"), (40, "Ddns"), (42, "Email"), (44, "Osd"),
    (46, "MotionDetect"), (52, "Shelter"), (54, "RecordCfg"), (56, "Compression"),
    (68, "Ftp"), (70, "FtpTask"), (76, "Ip"), (80, "VersionInfo"),
    (81, "RecordSchedule"), (93, "LinkType"), (97, "Upnp"), (101, "AutoReboot"),
    (102, "HddInfo"), (104, "SystemGeneral"), (106, "Dst"), (114, "Uid"), (115, "WifiSignal"), (116, "Wifi"),
    (120, "OnlineUsers"), (122, "Performance"), (124, "PushInfo"), (132, "VideoInputAdv"), (133, "RfAlarm"),
    (146, "StreamInfoList"), (151, "AbilityInfo"), (190, "PtzPreset"), (199, "Support"), (208, "LedState"),
    (212, "RfAlarmCfg"), (217, "EmailTask"), (219, "PushTask"), (228, "Crop"), (232, "AudioTask"),
    (252, "BatteryInfoList"), (253, "BatteryInfo"), (287, "TimeCfg"), (291, "Floodlight"), (299, "AiCfg"),
    (319, "Timelapse"), (342, "AiDetectCfg"),
    // Read ids recovered from the SDK command table (docs/baichuan-command-ids.md).
    (310, "EmailEnable"), (312, "FtpEnable"), (446, "FtpServerCfg"), (504, "DeviceAudioCfg"),
    (694, "PirMotionCfg"), (344, "AiDetectCfg2"), (87, "NetDisconnectExp"), (60, "AlarmIn"),
    // Getters behind official setters not yet in the app (setter-coverage audit, docs/settings-inventory.md).
    (439, "AiDenoise"), (195, "AutoUpdate"), (430, "Gdpr"), (210, "P2p"), (451, "PowerMode"), (537, "WifiSdb"),
    (264, "RingtoneCfg"), (427, "AutoReply"), (626, "BatteryMode"), (739, "AiEnhance"), (296, "DayNightThreshold"),
    (423, "PowerSavingTask"), (511, "AccessUser"), (363, "PushConfig"), (417, "BinoStitch"), (206, "CameraCfg"),
    // Rejected by Argus cameras; kept last because repeated rejections can end the session.
    (50, "VideoLoss"), (58, "AbilitySupport"), (78, "VideoInputDesc"), (83, "HandleException"),
];

fn redact(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<password>") {
        out.push_str(&rest[..i + 10]);
        rest = &rest[i + 10..];
        let end = rest.find("</password>").unwrap_or(rest.len());
        out.push_str("REDACTED");
        rest = &rest[end..];
    }
    out + rest
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output dir"));
    let path = std::path::PathBuf::from(std::env::var("APPDATA")?).join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    for d in saved["devices"].as_array().unwrap() {
        let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
        let cam = BcCamera::new(&BcCameraOpt {
            name: s("name"), channel_id: 0, addrs: s("host").parse().ok().into_iter().collect(),
            uid: Some(s("uid")).filter(|u| !u.is_empty()), port: None, protocol: ConnectionProtocol::TcpUdp,
            discovery: DiscoveryMethods::Local, max_discovery_retries: 5,
            credentials: Credentials { username: s("username"), password: Some(s("password")) }, debug: false,
        }).await?;
        cam.login().await?;
        let dir = out.join(s("name"));
        std::fs::create_dir_all(&dir)?;
        let mut ok = vec![];
        for &(id, name) in GETS {
            // Per-channel messages need the channel extension; device-level ones may reject it.
            let res = match cam.get_xml(id, true).await {
                Ok(x) => Ok(x),
                Err(_) => cam.get_xml(id, false).await,
            };
            match res {
                Ok(xml) => {
                    std::fs::write(dir.join(format!("{id:03}_{name}.xml")), redact(&xml))?;
                    ok.push(format!("{id}:{name}"));
                }
                Err(e) => println!("{:10} {id:3} {name:16} -> {e}", s("name")),
            }
        }
        println!("{:10} readable ({}): {}", s("name"), ok.len(), ok.join(" "));
    }
    Ok(())
}
