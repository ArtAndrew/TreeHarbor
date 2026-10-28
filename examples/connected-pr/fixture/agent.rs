// Scripted public-fixture agent. No LLM, credentials, checks or merge authority.
#![forbid(unsafe_code)]
use std::{fs, io::Write, path::Path, time::Duration};
const BASELINE: &str = include_str!("baseline.rs");
fn main() {
    assert!(std::env::current_dir().unwrap() == Path::new("/workspace"), "workspace required");
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() == 2, "one reviewed scenario required");
    let (before, after) = match args[1].as_str() {
        "A" => ("pub fn memory_per_worker_mb() -> u64 {\n    512\n}", "pub fn memory_per_worker_mb() -> u64 {\n    1024\n}"),
        "B" => ("pub fn worker_count() -> u64 {\n    4\n}", "pub fn worker_count() -> u64 {\n    8\n}"),
        _ => panic!("unknown scenario"),
    };
    let path = Path::new("src/lib.rs");
    let source = fs::read_to_string(path).unwrap();
    assert!(source == BASELINE, "unexpected pinned fixture source");
    let budget = "pub fn memory_budget_mb() -> u64 {\n    2048\n}";
    assert!(source.matches(before).count() == 1 && source.matches(budget).count() == 1);
    std::thread::sleep(Duration::from_secs(5));
    let next = source.replacen(before, after, 1).replacen(budget, "pub fn memory_budget_mb() -> u64 {\n    4096\n}", 1);
    let temporary = Path::new("src/.treeharbor-preview-edit");
    let mut file = fs::File::create_new(temporary).unwrap();
    file.write_all(next.as_bytes()).unwrap(); file.sync_all().unwrap(); drop(file);
    fs::rename(temporary, path).unwrap(); fs::File::open("src").unwrap().sync_all().unwrap();
    println!("scripted proposal complete");
}
