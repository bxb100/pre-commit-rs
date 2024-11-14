use clap::Parser;
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
struct RunnerCommand {
    #[arg()]
    cmd: Vec<String>,
}

fn main() {
    let cli = RunnerCommand::parse();

    // run pre-commit
    Command::new("pre-commit")
        .arg("run")
        .arg("--all-files")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .expect("failed to execute `pre-commit run --all-files`");

    let cmd = cli.cmd.first().unwrap();
    let mut command = Command::new(cmd);

    cli.cmd.iter().skip(1).for_each(|c| {
        command.arg(c);
    });

    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|_| panic!("failed to execute {}", cli.cmd.join(" ")));
}
