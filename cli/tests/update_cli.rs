use std::process::Command;

#[test]
fn update_commands_do_not_load_profiles_and_respect_package_managers() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "ahrara.exe"
    } else {
        "ahrara"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_ahrara"), &exe).unwrap();
    let missing_config = dir
        .path()
        .join("profile-that-must-not-be-created/config.toml");
    let invoke = |args: &[&str]| {
        Command::new(&exe)
            .arg("--config")
            .arg(&missing_config)
            .args(args)
            .output()
            .unwrap()
    };
    let output = invoke(&["update", "--register", "brew"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = invoke(&["--json", "update"]);
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["install_method"], "brew");
    assert_eq!(status["updated"], false);
    assert!(
        status["message"]
            .as_str()
            .unwrap()
            .contains("brew upgrade ahrara")
    );
    assert!(!missing_config.parent().unwrap().exists());
    assert!(!invoke(&["update", "--register", "manual"]).status.success());
    let output = invoke(&["update", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--check") && help.contains("--register"));
    assert!(
        !invoke(&["update", "--check", "--register", "manual"])
            .status
            .success()
    );
}
