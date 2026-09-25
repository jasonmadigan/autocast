#![cfg(unix)]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Recording {
    directory: PathBuf,
    success: bool,
    elapsed: Duration,
}

impl Recording {
    fn run(shell: Value, instructions: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "autocast-shell-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let mut recording = Self {
            directory,
            success: false,
            elapsed: Duration::ZERO,
        };
        let settings = json!({
            "width": 80, "height": 24, "timeout": "500ms", "shell": shell,
            "environment": [{"name": "TERM", "value": "xterm"}]
        });
        fs::write(
            recording.directory.join("input.yaml"),
            format!("settings: {settings}\ninstructions:\n{instructions}\n"),
        )
        .unwrap();
        let start = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_autocast"))
            .arg(recording.directory.join("input.yaml"))
            .arg(recording.directory.join("output.cast"))
            .stdout(Stdio::null())
            .stderr(fs::File::create(recording.directory.join("stderr")).unwrap())
            .spawn()
            .unwrap();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                recording.success = status.success();
                recording.elapsed = start.elapsed();
                return recording;
            }
            if start.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "autocast hung despite a 500ms timeout: {}",
                    recording.stderr()
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&self) -> String {
        fs::read_to_string(self.directory.join("stderr")).unwrap()
    }

    fn output(&self) -> String {
        assert!(self.success, "{}", self.stderr());
        let cast = fs::read_to_string(self.directory.join("output.cast")).unwrap();
        cast.lines()
            .skip(1)
            .map(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                event[2].as_str().unwrap().to_owned()
            })
            .collect()
    }

    fn assert_timeout(&self) {
        assert!(!self.success, "shell should not have exited successfully");
        let error = self.stderr();
        assert!(
            error.contains("timeout waiting for shell to stop"),
            "{error}"
        );
        assert!(self.elapsed >= Duration::from_millis(500));
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn custom_shell(script: &str, quit: Option<&str>) -> Value {
    let mut shell = json!({
        "program": "/bin/sh", "args": ["-c", script],
        "prompt": "READY", "line_split": ""
    });
    if let Some(quit) = quit {
        shell["quit_command"] = json!(quit);
    }
    shell
}

fn assert_clean_recording(shell: &str) {
    let recording = Recording::run(
        json!(shell),
        r"  - !Command
    command: echo hidden
    hidden: true
  - !Command
    command: echo test
  - !Command
    command: printf partial
  - !Command
    command: echo done",
    );
    assert_eq!(
        recording.output(),
        "$ echo test\r\ntest\r\n$ printf partial\r\npartial$ echo done\r\ndone\r\n$ \r\n"
    );
}

#[test]
fn bash_records_commands_once_and_exits() {
    assert_clean_recording("bash");
}

#[test]
fn zsh_records_commands_once_without_redraws() {
    assert_clean_recording("zsh");
}

#[test]
fn quit_times_out_when_shell_is_silent() {
    Recording::run(
        custom_shell(
            "printf READY; read line; while :; do sleep 1; done",
            Some("exit"),
        ),
        " []",
    )
    .assert_timeout();
}

#[test]
fn quit_times_out_when_shell_keeps_writing() {
    Recording::run(
        custom_shell(
            "printf READY; read line; while :; do printf output; done",
            Some("exit"),
        ),
        " []",
    )
    .assert_timeout();
}

#[test]
fn quit_times_out_without_a_quit_command() {
    Recording::run(custom_shell("printf READY; read line", None), " []").assert_timeout();
}

#[test]
fn quit_drains_more_than_a_terminal_buffer() {
    let recording = Recording::run(
        custom_shell(
            "printf READY; read line; dd if=/dev/zero bs=1024 count=256 2>/dev/null",
            Some("exit"),
        ),
        " []",
    );
    assert_eq!(recording.output(), "$ \r\n");
}

#[test]
fn quit_waits_for_process_exit_after_terminal_eof() {
    Recording::run(
        custom_shell(
            "printf READY; read line; exec 0<&- 1>&- 2>&-; while :; do sleep 1; done",
            Some("exit"),
        ),
        " []",
    )
    .assert_timeout();
}

#[test]
fn zsh_accepts_comments_and_multiline_commands() {
    let recording = Recording::run(
        json!("zsh"),
        r##"  - !Command
    command: "# demo comment"
  - !Command
    command:
      - echo multiline
      - command"##,
    );
    assert_eq!(
        recording.output(),
        "$ # demo comment\r\n$ echo multiline \\\r\n> command\r\nmultiline command\r\n$ \r\n"
    );
}

#[test]
fn zsh_preserves_interactive_input_and_application_escape_sequences() {
    let command = r#"read -r reply; printf '\033[31m%s\033[0m\n' "$reply""#;
    let recording = Recording::run(
        json!("zsh"),
        &format!(
            "  - !Interactive\n    command: {}\n    type_speed: 10ms\n    keys: [h, i, ^M]",
            serde_json::to_string(command).unwrap()
        ),
    );
    assert_eq!(
        recording.output(),
        format!("$ {command}\r\n\x1b[31mhi\x1b[0m\r\n$ \r\n")
    );
}

#[test]
fn quit_does_not_wait_for_a_descendant_to_close_the_terminal() {
    let recording = Recording::run(
        custom_shell(
            "printf READY; read line; trap '' HUP; sleep 10 &",
            Some("exit"),
        ),
        " []",
    );
    assert_eq!(recording.output(), "$ \r\n");
}
