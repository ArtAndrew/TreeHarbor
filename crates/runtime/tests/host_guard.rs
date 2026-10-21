use std::process::Command;

#[test]
fn refuses_to_execute_a_task_outside_namespace_init() {
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor-runtime-init"))
        .args([
            "--protocol",
            "1",
            "--timeout-ms",
            "1000",
            "--max-processes",
            "64",
            "--",
            "/this-command-must-never-run",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("requires PID 1") || error.contains("supports Linux only"));
}
