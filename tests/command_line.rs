#![cfg(all(feature = "client", feature = "server"))]
use std::process::{Command, Output};

fn run(server: bool, args: &[&str]) -> Output {
    Command::new(if server {
        env!("CARGO_BIN_EXE_chromie-server")
    } else {
        env!("CARGO_BIN_EXE_chromie")
    })
    .args(args)
    .env_remove("DATABASE_URL")
    .env_remove("CHROMIE_CLIENT_CONFIG")
    .env_remove("CHROMIE_CONFIG_DIR")
    .output()
    .unwrap()
}

#[test]
fn two_programs_expose_separate_help_without_loading_configuration() {
    let client = run(false, &[]);
    assert!(client.status.success());
    let help = String::from_utf8(client.stdout).unwrap();
    assert!(help.contains("init") && help.contains("secret") && help.contains("doctor"));
    for command in [
        "container-init",
        "container-serve",
        "initial-login",
        "create-space",
        "set-password",
    ] {
        assert!(!help.contains(command));
        assert!(!run(false, &[command]).status.success());
    }
    let server = run(true, &["--help"]);
    assert!(server.status.success());
    let help = String::from_utf8(server.stdout).unwrap();
    assert!(help.contains("container-serve") && help.contains("set-password"));
    for command in ["capture", "doctor", "secret", "hook"] {
        assert!(!run(true, &[command]).status.success());
    }
    for server in [false, true] {
        assert!(run(server, &["--version"]).status.success());
    }
}

#[test]
fn nested_help_explains_options_and_argument_relations() {
    for args in [
        vec!["init", "--help"],
        vec!["hook", "install", "--help"],
        vec!["secret", "put", "--help"],
        vec!["secret", "exec", "--help"],
        vec!["help", "secret", "get"],
    ] {
        let result = run(false, &args);
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("用法：") && help.contains("选项"));
        assert!(!help.contains("possible values:") && !help.contains("[OPTIONS]"));
    }
    let result = run(false, &["secret", "put", "--help"]);
    let help = String::from_utf8(result.stdout).unwrap();
    assert!(help.contains("--prompt") && help.contains("--file") && help.contains("--label"));
}

#[test]
fn invalid_inputs_are_rejected_without_echoing_user_values() {
    let marker = "fixture-sensitive-must-not-be-echoed";
    for args in [
        vec![marker],
        vec!["secret", "put", "--password", marker],
        vec!["secret", "get", "--ref", marker],
        vec![
            "secret",
            "get",
            "--ref",
            "secret://demo/fixture",
            "--out",
            "/tmp/fixture",
            "--revision",
            marker,
        ],
        vec![
            "secret",
            "put",
            "--label",
            marker,
            "--prompt",
            "--file",
            "/tmp/fixture",
        ],
        vec!["init", "--url", marker, "--url", marker, "--space", "demo"],
        vec!["init", "--agent", marker],
    ] {
        let result = run(false, &args);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("错误：") && error.contains("用法："));
        assert!(!error.contains(marker));
    }
    let result = run(true, &["set-password", marker]);
    assert_eq!(result.status.code(), Some(2));
    assert!(!String::from_utf8(result.stderr).unwrap().contains(marker));
}
