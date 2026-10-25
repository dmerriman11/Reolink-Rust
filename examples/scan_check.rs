//! Run the LAN camera search a few times and print what answers: `cargo run --example scan_check`
#[path = "../src/scan.rs"]
mod scan;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    for round in 1..=3 {
        let t = std::time::Instant::now();
        let found = scan::scan(std::time::Duration::from_secs(3)).await?;
        let list: Vec<String> = found.iter().map(|f| format!("{} {}…", f.ip, &f.uid[..8.min(f.uid.len())])).collect();
        println!("round {round}: {} found in {:.1}s: {}", found.len(), t.elapsed().as_secs_f32(), list.join(", "));
    }
    Ok(())
}
