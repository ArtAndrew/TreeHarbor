//! Real-runtime test driver, not the service CLI or an agent-facing endpoint.
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io::Read, path::PathBuf};
use treeharbor_runtime::{
    Controller, Definition, Engine, Inspection, Outcome, Profile, Specification, Task, Volume,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    socket: PathBuf,
    journal: PathBuf,
    mount: PathBuf,
    files: PathBuf,
    attempt: Uuid,
    profile: Definition,
    argv: Vec<String>,
}
fn observed(i: Inspection) -> Value {
    json!({"id":i.container.id(),"state":format!("{:?}",i.state).to_lowercase(),
    "started_at":i.started_at,"finished_at":i.finished_at,"exit_code":i.exit_code,"oom_killed":i.oom_killed})
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("input required")?);
    let action = args.next().ok_or("action required")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 128 * 1024 {
        return Err("input too large".into());
    }
    let input: Input = serde_json::from_slice(&bytes)?;
    let profile = Profile::new(input.profile)?;
    let volume = Volume::open(
        &input.mount,
        &input.files,
        profile.definition().workspace_bytes,
    )?;
    let engine = Engine::open(&input.socket).await?;
    let spec = Specification::new(
        input.attempt,
        [42; 32],
        profile,
        Task::new(input.argv)?,
        volume,
    )?;
    let controller = Controller::open(&input.journal, &engine, spec)?;
    let result = match action.to_str().ok_or("invalid action")? {
        "create" => observed(controller.create().await?),
        "start" => observed(controller.start().await?),
        "status" => match controller.status().await? {
            Outcome::NotCreated => json!({"state":"new"}),
            Outcome::Removed => json!({"state":"removed"}),
            Outcome::Observed(i) => observed(i),
        },
        "remove" => {
            controller.remove().await?;
            json!({"state":"removed"})
        }
        _ => return Err("invalid action".into()),
    };
    println!("{result}");
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("runtime controller: {e}");
        std::process::exit(1);
    }
}
