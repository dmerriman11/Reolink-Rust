//! Measurement: when each video frame reaches the app, against the camera's own clock.
//! `cargo run --example stream_timing -- "<device name>" [main|sub] [secs]`
//! RUST_LOG=neolink_core::bc_protocol::connection::udpsource=debug adds the UDP loss and hole counters.
use neolink_core::bc_protocol::{BcCamera, BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods, StreamKind};
use neolink_core::bcmedia::model::BcMedia;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args = std::env::args().skip(1);
    let want = args.next().expect("camera name");
    let kind = if args.next().as_deref() == Some("sub") { StreamKind::Sub } else { StreamKind::Main };
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    let path = dirs::config_dir().unwrap().join("Reolink-rs/devices.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let d = saved["devices"].as_array().unwrap().iter().find(|d| d["name"] == want.as_str()).expect("no such camera");
    let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
    let t0 = Instant::now();
    let cam = BcCamera::new(&BcCameraOpt {
        name: s("name"),
        channel_id: 0,
        addrs: s("host").parse().ok().into_iter().collect(),
        uid: Some(s("uid")).filter(|u| !u.is_empty()),
        port: None,
        protocol: ConnectionProtocol::TcpUdp,
        discovery: DiscoveryMethods::Relay, // what the app uses: LAN first, then the relay
        max_discovery_retries: 10,
        credentials: Credentials { username: s("username"), password: Some(password(&s("secret"), s("password"))) },
        debug: false,
    })
    .await?;
    cam.login().await?;
    println!("{want} {kind}: connected in {:.1}s", t0.elapsed().as_secs_f32());

    let mut stream = cam.start_video(kind, 0, false).await?;
    let start = Instant::now();
    let mut frames: Vec<(f64, f64, bool, usize)> = vec![]; // arrival ms, camera ms, key, bytes
    let mut camera_us: Option<(u32, f64)> = None;
    while start.elapsed() < Duration::from_secs(secs) {
        let media = match tokio::time::timeout(Duration::from_secs(5), stream.get_data()).await {
            Ok(m) => m??,
            Err(_) => continue,
        };
        let (us, key, n) = match &media {
            BcMedia::Iframe(f) => (f.microseconds, true, f.data.len()),
            BcMedia::Pframe(f) => (f.microseconds, false, f.data.len()),
            _ => continue,
        };
        let (last, total) = camera_us.unwrap_or((us, 0.0));
        let total = total + us.wrapping_sub(last) as i32 as f64 / 1000.0;
        camera_us = Some((us, total));
        frames.push((start.elapsed().as_secs_f64() * 1000.0, total, key, n));
    }
    let _ = stream.shutdown().await;
    report(&frames);
    Ok(())
}

fn report(frames: &[(f64, f64, bool, usize)]) {
    assert!(frames.len() > 1, "the camera sent no video");
    let (first, last) = (frames[0], frames[frames.len() - 1]);
    let secs = (last.0 - first.0) / 1000.0;
    let camera_secs = (last.1 - first.1) / 1000.0;
    let keys: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| f.2).map(|(i, _)| i).collect();
    let bytes: usize = frames.iter().map(|f| f.3).sum();
    println!(
        "{} frames, {} keyframes, {:.0} kbit/s; {:.2} fps by arrival, {:.2} fps by camera clock; camera ran {:.1}s in {:.1}s",
        frames.len(), keys.len(), bytes as f64 * 8.0 / 1000.0 / secs, (frames.len() - 1) as f64 / secs,
        (frames.len() - 1) as f64 / camera_secs.max(0.001), camera_secs, secs
    );
    if keys.len() > 1 {
        let avg_key: usize = keys.iter().map(|&i| frames[i].3).sum::<usize>() / keys.len();
        println!("keyframe every {:.2}s (camera clock), average {} KB", (frames[keys[keys.len() - 1]].1 - frames[keys[0]].1) / 1000.0 / (keys.len() - 1) as f64, avg_key / 1024);
    }
    let arrival: Vec<f64> = frames.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let mut camera: Vec<f64> = frames.windows(2).map(|w| w[1].1 - w[0].1).collect();
    println!("gap between frames, arrival: {}", percentiles(&mut arrival.clone()));
    println!("gap between frames, camera:  {}", percentiles(&mut camera));
    // How late each frame is against the earliest it could be: what a player's buffer has to absorb.
    let floor = frames.iter().map(|f| f.0 - f.1).fold(f64::MAX, f64::min);
    let mut late: Vec<f64> = frames.iter().map(|f| f.0 - f.1 - floor).collect();
    println!("lateness against the camera clock: {}", percentiles(&mut late));
    let stalls: Vec<(usize, f64)> = frames.windows(2).enumerate().filter(|(_, w)| w[1].0 - w[0].0 > 300.0).map(|(i, w)| (i, w[1].0 - w[0].0)).collect();
    let near_key = stalls.iter().filter(|(i, _)| keys.iter().any(|&k| k.abs_diff(*i + 1) <= 1)).count();
    println!("stalls over 300 ms: {} (longest {:.0} ms), {} next to a keyframe", stalls.len(), stalls.iter().map(|s| s.1).fold(0.0, f64::max), near_key);
    let rows: Vec<String> = frames.iter().map(|f| format!("{:.1},{:.1},{},{}", f.0, f.1, f.2 as u8, f.3)).collect();
    let csv = std::env::temp_dir().join(format!("stream_timing_{}.csv", std::process::id()));
    if std::fs::write(&csv, rows.join("\n")).is_ok() {
        println!("per-frame data: {}", csv.display());
    }
}


fn percentiles(v: &mut [f64]) -> String {
    v.sort_by(f64::total_cmp);
    let p = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
    format!("p50 {:.0} ms, p95 {:.0}, p99 {:.0}, max {:.0}", p(0.5), p(0.95), p(0.99), p(1.0))
}

/// A camera's password from the OS credential store (see src/secrets.rs), else the one still in devices.json.
fn password(secret: &str, in_file: String) -> String {
    keyring::Entry::new("Reolink-rs", secret).and_then(|e| e.get_password()).unwrap_or(in_file)
}
