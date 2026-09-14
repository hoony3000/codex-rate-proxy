//! Shell commands for explicitly exporting a registered credential.
use super::*;
use std::io::IsTerminal;

pub(super) fn run(args: &[String]) -> Result<()> {
    let mut user = None;
    let mut shell = None;
    let mut variable = None;
    let mut i = 0;
    while i < args.len() {
        let target = match args[i].as_str() {
            "-u" | "--user" => &mut user,
            "--shell" => &mut shell,
            "--var" => &mut variable,
            "-h" | "--help" if args.len() == 1 => {
                println!("Usage: codex-rate-proxy env -u NAME --shell bash|csh|tcsh [--var OPENAI_API_KEY]\nOutputs a secret-bearing shell command; use only through shell evaluation, never logs.");
                return Ok(());
            }
            _ => return Err("unknown env option; use env --help".into()),
        };
        if target.is_some() { return Err("duplicate env option".into()); }
        i += 1;
        *target = Some(args.get(i).ok_or("env option requires a value")?.as_str());
        i += 1;
    }
    let user = user.ok_or("env requires --user NAME or -u NAME")?;
    let shell = shell.ok_or("env requires --shell bash|csh|tcsh")?;
    if !matches!(shell, "bash" | "csh" | "tcsh") { return Err("unsupported shell; use bash, csh or tcsh".into()); }
    let variable = variable.unwrap_or("OPENAI_API_KEY");
    let mut bytes = variable.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err("--var must be a shell identifier: letters, digits, underscore; no leading digit".into());
    }
    if std::io::stdout().is_terminal() {
        return Err("refusing to print a credential to the terminal; use shell evaluation (see README)".into());
    }
    let key = credentials::registered_key(user)?;
    validate_key(&key)?;
    let command = shell_command(shell, variable, &key);
    // Emit nothing until all validation/decryption succeeds.
    std::io::stdout().lock().write_all(command.as_bytes())?;
    Ok(())
}

fn shell_command(shell: &str, variable: &str, key: &str) -> String {
    // No literal key metacharacters enter eval. Octal is encoding, NOT encryption.
    let octal: String = key.bytes().map(|b| format!("\\{b:03o}")).collect();
    if shell == "bash" {
        format!("export {variable}=$'{octal}'\n")
    } else {
        // Quoted command substitution preserves all bytes, including !, quotes and backticks.
        // /usr/bin/printf is part of coreutils on the supported CentOS environment.
        format!("setenv {variable} \"`/usr/bin/printf '{octal}'`\"\n")
    }
}
