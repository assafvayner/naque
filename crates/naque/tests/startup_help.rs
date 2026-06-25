//! End-to-end startup behavior when no database connection is configured.
//!
//! Each case runs the real `naque` binary in an isolated environment — no
//! `DATABASE_URL`, an empty `HOME` (so `~/.naque` resolves nowhere), and a
//! throwaway working directory (so no `naque.toml` is discovered).

use std::path::PathBuf;
use std::process::{Command, Output};

/// A fresh, empty directory unique to this test invocation.
fn isolated_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("naque-startup-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Invoke the built binary with a clean, connection-free environment.
fn run(tag: &str, extra_args: &[&str]) -> Output {
    let dir = isolated_dir(tag);
    Command::new(env!("CARGO_BIN_EXE_naque"))
        .args(extra_args)
        .env_remove("DATABASE_URL")
        .env("HOME", &dir)
        .env("NO_COLOR", "1") // deterministic, escape-free output
        .current_dir(&dir)
        .output()
        .expect("run naque")
}

#[test]
fn bare_launch_prints_friendly_guidance_to_stdout_and_exits_zero() {
    let out = run("bare", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "bare launch should exit 0: {:?}", out);
    assert!(stdout.contains("agentic AI query tool"), "expected getting-started banner on stdout, got: {stdout}");
    assert!(stdout.contains("--url"));
    assert!(stdout.contains("DATABASE_URL"));
    assert!(stdout.contains("naque --help"));
    // Friendly, not an error.
    assert!(!stdout.contains("Error:"));
}

#[test]
fn misconfigured_launch_prints_error_to_stderr_and_exits_nonzero() {
    // A non-connection flag means the launch isn't "bare" — so a missing
    // connection is an error rather than first-run guidance.
    let out = run("error", &["--mode", "readonly"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "should exit non-zero: {:?}", out);
    assert!(stderr.contains("no database connection configured"), "expected error on stderr, got: {stderr}");
    assert!(stderr.contains("Set one of:"));
    assert!(stderr.contains("Run `naque --help`"));
}
