use std::collections::BTreeSet;

fn flags(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| {
            word.strip_prefix("--")
                .is_some_and(|name| name.starts_with(|c: char| c.is_ascii_lowercase()))
        })
}

#[test]
fn cli_reference_matches_help() {
    let readme = include_str!("../README.md");
    let agent_commands = readme
        .lines()
        .map(str::trim_end)
        .skip_while(|line| *line != "## Agent commands")
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!agent_commands.is_empty(), "Missing README section: Agent commands");

    let outputs = include_str!("../../docs/cli-outputs.md");
    let first_column = outputs
        .lines()
        .map(str::trim_start)
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| line[1..].split('|').next());
    let output_commands: BTreeSet<_> = first_column.flat_map(flags).collect();

    let usage = super::usage();
    let help_flags: BTreeSet<_> = flags(&usage).collect();
    let readme_flags: BTreeSet<_> = flags(&agent_commands).collect();
    // The first flag in each help invocation is the command. A leading
    // --headless switches rendering mode, so skip it when looking for a command.
    // Plain-text commands also get rows so new commands are checked by default.
    let commands: BTreeSet<_> = usage
        .lines()
        .filter_map(|line| line.split_once("tak_viewer "))
        .filter_map(|(_, args)| {
            args.split_whitespace()
                .map(|word| word.trim_matches(['[', ']']))
                .skip_while(|word| *word == "--headless")
                .next()
                .and_then(|word| flags(word).next())
        })
        .collect();

    let extra_flags = [
        "--ignore", // checks/compare_png.py option, not a tak_viewer flag.
    ];
    let undocumented: Vec<_> = help_flags.difference(&readme_flags).copied().collect();
    let missing_outputs: Vec<_> = commands.difference(&output_commands).copied().collect();
    let stale: Vec<_> = readme_flags
        .difference(&help_flags)
        .copied()
        .filter(|flag| !extra_flags.contains(flag))
        .collect();

    let mut errors = Vec::new();
    for (label, entries) in [
        ("Help flags missing from README Agent commands", undocumented),
        ("Commands missing from the first CLI outputs table", missing_outputs),
        ("README Agent command flags missing from help", stale),
    ] {
        if !entries.is_empty() {
            errors.push(format!("{label}: {}", entries.join(", ")));
        }
    }
    assert!(errors.is_empty(), "CLI reference drift:\n{}", errors.join("\n"));
}
