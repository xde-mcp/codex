use crate::bash::try_parse_bash;
use crate::bash::try_parse_word_only_commands_sequence;
use serde::Deserialize;
use serde::Serialize;
use shlex::split as shlex_split;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum ParsedCommand {
    Read {
        cmd: Vec<String>,
        name: String,
    },
    Ls {
        cmd: Vec<String>,
        path: Option<String>,
    },
    Search {
        cmd: Vec<String>,
        query: Option<String>,
        path: Option<String>,
        files_only: bool,
    },
    Format {
        cmd: Vec<String>,
        tool: Option<String>,
        targets: Option<Vec<String>>,
    },
    Test {
        cmd: Vec<String>,
        runner: Option<String>,
        test_filter: Option<Vec<String>>,
    },
    Lint {
        cmd: Vec<String>,
        tool: Option<String>,
        targets: Option<Vec<String>>,
    },
    Unknown {
        cmd: Vec<String>,
    },
}

pub fn parse_command(command: &[String]) -> Vec<ParsedCommand> {
    let normalized = normalize_tokens(command);

    if let Some(commands) = parse_bash_lc_commands(command, &normalized) {
        return commands;
    }

    let parts = if contains_connectors(&normalized) {
        split_on_connectors(&normalized)
    } else {
        vec![normalized.clone()]
    };

    // Map each pipeline segment to its parsed summary.
    let mut parsed: Vec<ParsedCommand> = parts
        .iter()
        .map(|tokens| summarize_main_tokens(tokens))
        .collect();

    // If a pipeline ends with `nl` using only flags (e.g., `| nl -ba`), drop it so the
    // main action (e.g., a sed range over a file) is surfaced cleanly.
    if parsed.len() >= 2 {
        parsed.retain(|pc| {
            match pc {
                ParsedCommand::Unknown { cmd } => {
                    if let Some(first) = cmd.first() {
                        if first == "nl" {
                            // Treat `nl` without an explicit file operand as formatting-only.
                            return cmd.iter().skip(1).any(|a| !a.starts_with('-'));
                        }
                    }
                    true
                }
                _ => true,
            }
        });
    }

    parsed
}

/// Validates that this is a `sed -n 123,123p` command.
fn is_valid_sed_n_arg(arg: Option<&str>) -> bool {
    let s = match arg {
        Some(s) => s,
        None => return false,
    };
    let core = match s.strip_suffix('p') {
        Some(rest) => rest,
        None => return false,
    };
    let parts: Vec<&str> = core.split(',').collect();
    match parts.as_slice() {
        [num] => !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()),
        [a, b] => {
            !a.is_empty()
                && !b.is_empty()
                && a.chars().all(|c| c.is_ascii_digit())
                && b.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Normalize a command by:
/// - Removing `yes`/`no`/`bash -c`/`bash -lc` prefixes.
/// - Splitting on `|` and `&&`/`||`/`;
fn normalize_tokens(cmd: &[String]) -> Vec<String> {
    match cmd {
        [first, pipe, rest @ ..] if (first == "yes" || first == "y") && pipe == "|" => {
            let s = rest.join(" ");
            shlex_split(&s).unwrap_or_else(|| rest.to_vec())
        }
        [first, pipe, rest @ ..] if (first == "no" || first == "n") && pipe == "|" => {
            let s = rest.join(" ");
            shlex_split(&s).unwrap_or_else(|| rest.to_vec())
        }
        [bash, flag, script] if bash == "bash" && (flag == "-c" || flag == "-lc") => {
            shlex_split(script)
                .unwrap_or_else(|| vec!["bash".to_string(), flag.clone(), script.clone()])
        }
        _ => cmd.to_vec(),
    }
}

fn contains_connectors(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|t| t == "&&" || t == "||" || t == "|" || t == ";")
}

fn split_on_connectors(tokens: &[String]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for t in tokens {
        if t == "&&" || t == "||" || t == "|" || t == ";" {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(t.clone());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn trim_at_connector(tokens: &[String]) -> Vec<String> {
    let idx = tokens
        .iter()
        .position(|t| t == "|" || t == "&&" || t == "||")
        .unwrap_or(tokens.len());
    tokens[..idx].to_vec()
}

/// Shorten a path to the last component, excluding `build`/`dist`/`node_modules`/`src`.
/// It also pulls out a useful path from a directory such as:
/// - webview/src -> webview
/// - foo/src/ -> foo
/// - packages/app/node_modules/ -> app
fn short_display_path(path: &str) -> String {
    let mut parts = path.split('/').rev().filter(|p| {
        !p.is_empty() && *p != "build" && *p != "dist" && *p != "node_modules" && *p != "src"
    });
    parts
        .next()
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
}

fn collect_non_flag_targets(args: &[String]) -> Option<Vec<String>> {
    let mut targets = Vec::new();
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if a == "--" {
            break;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-p"
            || a == "--package"
            || a == "--features"
            || a == "-C"
            || a == "--config"
            || a == "--config-path"
            || a == "--out-dir"
            || a == "-o"
            || a == "--run"
            || a == "--max-warnings"
            || a == "--format"
        {
            if i + 1 < args.len() {
                skip_next = true;
            }
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        targets.push(a.clone());
    }
    if targets.is_empty() {
        None
    } else {
        Some(targets)
    }
}

fn parse_cargo_test_filter(args: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if a == "--" {
            break;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-p" || a == "--package" {
            if let Some(val) = args.get(i + 1) {
                out.push(val.clone());
            }
            if i + 1 < args.len() {
                skip_next = true;
            }
            continue;
        }
        if let Some(rest) = a.strip_prefix("--package=") {
            if !rest.is_empty() {
                out.push(rest.to_string());
            }
            continue;
        }
        if a == "--features" {
            if i + 1 < args.len() {
                skip_next = true;
            }
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        out.push(a.clone());
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_pytest_filters(args: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut next_is_k = false;
    for a in args {
        if next_is_k {
            out.push(a.clone());
            next_is_k = false;
            continue;
        }
        if a == "-k" || a == "--keyword" {
            next_is_k = true;
            continue;
        }
        if !a.starts_with('-') && (a.ends_with(".py") || a.contains("::")) {
            out.push(a.clone());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_jest_vitest_filters(args: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut next_is_t = false;
    for a in args {
        if next_is_t {
            out.push(a.clone());
            next_is_t = false;
            continue;
        }
        if a == "-t" || a == "--testNamePattern" {
            next_is_t = true;
            continue;
        }
        if !a.starts_with('-')
            && (a.ends_with(".ts")
                || a.ends_with(".tsx")
                || a.ends_with(".js")
                || a.ends_with(".jsx"))
        {
            out.push(a.clone());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn parse_go_test_filters(args: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut next_is_run = false;
    for a in args {
        if next_is_run {
            out.push(a.clone());
            next_is_run = false;
            continue;
        }
        if a == "-run" {
            next_is_run = true;
            continue;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn classify_npm_like(tool: &str, tail: &[String], full_cmd: &[String]) -> Option<ParsedCommand> {
    let mut r = tail;
    if tool == "pnpm" && r.first().map(|s| s.as_str()) == Some("-r") {
        r = &r[1..];
    }
    let mut script_name: Option<String> = None;
    if r.first().map(|s| s.as_str()) == Some("run") {
        script_name = r.get(1).cloned();
    } else {
        let is_test_cmd = (tool == "npm" && r.first().map(|s| s.as_str()) == Some("t"))
            || ((tool == "npm" || tool == "pnpm" || tool == "yarn")
                && r.first().map(|s| s.as_str()) == Some("test"));
        if is_test_cmd {
            script_name = Some("test".to_string());
        }
    }
    if let Some(name) = script_name {
        let lname = name.to_lowercase();
        if lname == "test" || lname == "unit" || lname == "jest" || lname == "vitest" {
            return Some(ParsedCommand::Test {
                cmd: full_cmd.to_vec(),
                runner: Some(format!("{tool}-script")),
                test_filter: None,
            });
        }
        if lname == "lint" || lname == "eslint" {
            return Some(ParsedCommand::Lint {
                cmd: full_cmd.to_vec(),
                tool: Some(format!("{tool}-script:{name}")),
                targets: None,
            });
        }
        if lname == "format" || lname == "fmt" || lname == "prettier" {
            return Some(ParsedCommand::Format {
                cmd: full_cmd.to_vec(),
                tool: Some(format!("{tool}-script:{name}")),
                targets: None,
            });
        }
    }
    None
}

fn parse_bash_lc_commands(
    original: &[String],
    normalized: &[String],
) -> Option<Vec<ParsedCommand>> {
    let [bash, flag, script] = original else {
        return None;
    };
    if bash != "bash" || flag != "-lc" {
        return None;
    }
    if let Some(tree) = try_parse_bash(script) {
        if let Some(all_commands) = try_parse_word_only_commands_sequence(&tree, script) {
            if !all_commands.is_empty() {
                let script_tokens = shlex_split(script)
                    .unwrap_or_else(|| vec!["bash".to_string(), flag.clone(), script.clone()]);
                // Strip small formatting helpers (e.g., head/tail/awk/wc/etc) so we
                // bias toward the primary command when pipelines are present.
                // First, drop obvious small formatting helpers (e.g., wc/awk/etc).
                let filtered_commands = drop_small_formatting_commands(all_commands);
                if filtered_commands.is_empty() {
                    return Some(vec![ParsedCommand::Unknown {
                        cmd: normalized.to_vec(),
                    }]);
                }
                let mut commands: Vec<ParsedCommand> = filtered_commands
                    .into_iter()
                    .map(|tokens| match summarize_main_tokens(&tokens) {
                        ParsedCommand::Ls { path, .. } => ParsedCommand::Ls {
                            cmd: script_tokens.clone(),
                            path,
                        },
                        other => other,
                    })
                    .collect();
                commands = maybe_collapse_cat_sed(commands, &script_tokens);
                if commands.len() == 1 {
                    // If we reduced to a single command, attribute the full original script
                    // for clearer UX in file-reading and listing scenarios, or when there were
                    // no connectors in the original script. For search commands that came from
                    // a pipeline (e.g. `rg --files | sed -n`), keep only the primary command.
                    let had_connectors = script_tokens
                        .iter()
                        .any(|t| t == "|" || t == "&&" || t == "||" || t == ";");
                    commands = commands
                        .into_iter()
                        .map(|pc| match pc {
                            ParsedCommand::Read { name, .. } => ParsedCommand::Read {
                                cmd: script_tokens.clone(),
                                name,
                            },
                            ParsedCommand::Ls { path, .. } => ParsedCommand::Ls {
                                cmd: script_tokens.clone(),
                                path,
                            },
                            ParsedCommand::Search {
                                cmd,
                                query,
                                path,
                                files_only,
                            } => {
                                if had_connectors {
                                    ParsedCommand::Search {
                                        cmd,
                                        query,
                                        path,
                                        files_only,
                                    }
                                } else {
                                    ParsedCommand::Search {
                                        cmd: script_tokens.clone(),
                                        query,
                                        path,
                                        files_only,
                                    }
                                }
                            }
                            ParsedCommand::Format { tool, targets, .. } => ParsedCommand::Format {
                                cmd: script_tokens.clone(),
                                tool,
                                targets,
                            },
                            ParsedCommand::Test {
                                runner,
                                test_filter,
                                ..
                            } => ParsedCommand::Test {
                                cmd: script_tokens.clone(),
                                runner,
                                test_filter,
                            },
                            ParsedCommand::Lint { tool, targets, .. } => ParsedCommand::Lint {
                                cmd: script_tokens.clone(),
                                tool,
                                targets,
                            },
                            ParsedCommand::Unknown { .. } => ParsedCommand::Unknown {
                                cmd: script_tokens.clone(),
                            },
                        })
                        .collect();
                }
                return Some(commands);
            }
        }
    }
    Some(vec![ParsedCommand::Unknown {
        cmd: normalized.to_vec(),
    }])
}

/// Return true if this looks like a small formatting helper in a pipeline.
/// Examples: `head -n 40`, `tail -n +10`, `wc -l`, `awk ...`, `cut ...`, `tr ...`.
/// We try to keep variants that clearly include a file path (e.g. `tail -n 30 file`).
fn is_small_formatting_command(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let cmd = tokens[0].as_str();
    match cmd {
        // Always formatting; typically used in pipes.
        // `nl` is special-cased below to allow `nl <file>` to be treated as a read command.
        "wc" | "tr" | "cut" | "sort" | "uniq" | "xargs" | "tee" | "column" | "awk" | "yes"
        | "printf" => true,
        "head" => {
            // Treat as formatting when no explicit file operand is present.
            // Common forms: `head -n 40`, `head -c 100`.
            // Keep cases like `head -n 40 file`.
            tokens.len() < 3
        }
        "tail" => {
            // Treat as formatting when no explicit file operand is present.
            // Common forms: `tail -n +10`, `tail -n 30`.
            // Keep cases like `tail -n 30 file`.
            tokens.len() < 3
        }
        "sed" => {
            // Keep `sed -n <range> file` (treated as a file read elsewhere);
            // otherwise consider it a formatting helper in a pipeline.
            tokens.len() < 4
                || !(tokens[1] == "-n" && is_valid_sed_n_arg(tokens.get(2).map(|s| s.as_str())))
        }
        _ => false,
    }
}

fn drop_small_formatting_commands(mut commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    commands.retain(|tokens| !is_small_formatting_command(tokens));
    commands
}

fn maybe_collapse_cat_sed(
    commands: Vec<ParsedCommand>,
    script_tokens: &[String],
) -> Vec<ParsedCommand> {
    if commands.len() < 2 {
        return commands;
    }
    let drop_leading_sed = match (&commands[0], &commands[1]) {
        (ParsedCommand::Unknown { cmd: sed_cmd }, ParsedCommand::Read { cmd: cat_cmd, .. }) => {
            let is_sed_n = sed_cmd.first().map(|s| s.as_str()) == Some("sed")
                && sed_cmd.get(1).map(|s| s.as_str()) == Some("-n")
                && is_valid_sed_n_arg(sed_cmd.get(2).map(|s| s.as_str()))
                && sed_cmd.len() == 3;
            let is_cat_file =
                cat_cmd.first().map(|s| s.as_str()) == Some("cat") && cat_cmd.len() == 2;
            is_sed_n && is_cat_file
        }
        _ => false,
    };
    if drop_leading_sed {
        if let ParsedCommand::Read { name, .. } = &commands[1] {
            return vec![ParsedCommand::Read {
                cmd: script_tokens.to_vec(),
                name: name.clone(),
            }];
        }
    }
    commands
}

fn summarize_main_tokens(main_cmd: &[String]) -> ParsedCommand {
    match main_cmd.split_first() {
        // sed -n '<range>' <file> | ...  -> treat as a search targeting <file>.
        // This is commonly used with `nl -ba` in a pipeline for line numbering.
        Some((head, tail)) if head == "sed" => {
            if tail.get(0).map(|s| s.as_str()) == Some("-n")
                && is_valid_sed_n_arg(tail.get(1).map(|s| s.as_str()))
                && tail.len() >= 3
            {
                // Use the last non-flag argument as the file operand.
                let file = tail.iter().rev().find(|s| !s.starts_with('-')).cloned();
                if let Some(p) = file {
                    return ParsedCommand::Search {
                        cmd: main_cmd.to_vec(),
                        query: None,
                        path: Some(short_display_path(&p)),
                        files_only: false,
                    };
                }
            }
            ParsedCommand::Unknown {
                cmd: main_cmd.to_vec(),
            }
        }
        Some((head, tail)) if head == "nl" => {
            let path = tail.iter().find(|p| !p.starts_with('-'));
            if let Some(p) = path {
                let name = short_display_path(p);
                ParsedCommand::Read {
                    cmd: main_cmd.to_vec(),
                    name,
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: main_cmd.to_vec(),
                }
            }
        }
        Some((head, tail))
            if head == "cargo" && tail.first().map(|s| s.as_str()) == Some("fmt") =>
        {
            ParsedCommand::Format {
                cmd: main_cmd.to_vec(),
                tool: Some("cargo fmt".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail))
            if head == "cargo" && tail.first().map(|s| s.as_str()) == Some("clippy") =>
        {
            ParsedCommand::Lint {
                cmd: main_cmd.to_vec(),
                tool: Some("cargo clippy".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail))
            if head == "cargo" && tail.first().map(|s| s.as_str()) == Some("test") =>
        {
            ParsedCommand::Test {
                cmd: main_cmd.to_vec(),
                runner: Some("cargo".to_string()),
                test_filter: parse_cargo_test_filter(&tail[1..]),
            }
        }
        Some((head, tail)) if head == "rustfmt" => ParsedCommand::Format {
            cmd: main_cmd.to_vec(),
            tool: Some("rustfmt".to_string()),
            targets: collect_non_flag_targets(tail),
        },
        Some((head, tail)) if head == "go" && tail.first().map(|s| s.as_str()) == Some("fmt") => {
            ParsedCommand::Format {
                cmd: main_cmd.to_vec(),
                tool: Some("go fmt".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail)) if head == "go" && tail.first().map(|s| s.as_str()) == Some("test") => {
            ParsedCommand::Test {
                cmd: main_cmd.to_vec(),
                runner: Some("go".to_string()),
                test_filter: parse_go_test_filters(&tail[1..]),
            }
        }
        Some((head, tail)) if head == "pytest" => ParsedCommand::Test {
            cmd: main_cmd.to_vec(),
            runner: Some("pytest".to_string()),
            test_filter: parse_pytest_filters(tail),
        },
        Some((head, tail)) if head == "eslint" => ParsedCommand::Lint {
            cmd: main_cmd.to_vec(),
            tool: Some("eslint".to_string()),
            targets: collect_non_flag_targets(tail),
        },
        Some((head, tail)) if head == "prettier" => ParsedCommand::Format {
            cmd: main_cmd.to_vec(),
            tool: Some("prettier".to_string()),
            targets: collect_non_flag_targets(tail),
        },
        Some((head, tail)) if head == "black" => ParsedCommand::Format {
            cmd: main_cmd.to_vec(),
            tool: Some("black".to_string()),
            targets: collect_non_flag_targets(tail),
        },
        Some((head, tail))
            if head == "ruff" && tail.first().map(|s| s.as_str()) == Some("check") =>
        {
            ParsedCommand::Lint {
                cmd: main_cmd.to_vec(),
                tool: Some("ruff".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail))
            if head == "ruff" && tail.first().map(|s| s.as_str()) == Some("format") =>
        {
            ParsedCommand::Format {
                cmd: main_cmd.to_vec(),
                tool: Some("ruff".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail)) if (head == "jest" || head == "vitest") => ParsedCommand::Test {
            cmd: main_cmd.to_vec(),
            runner: Some(head.clone()),
            test_filter: parse_jest_vitest_filters(tail),
        },
        Some((head, tail))
            if head == "npx" && tail.first().map(|s| s.as_str()) == Some("eslint") =>
        {
            ParsedCommand::Lint {
                cmd: main_cmd.to_vec(),
                tool: Some("eslint".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        Some((head, tail))
            if head == "npx" && tail.first().map(|s| s.as_str()) == Some("prettier") =>
        {
            ParsedCommand::Format {
                cmd: main_cmd.to_vec(),
                tool: Some("prettier".to_string()),
                targets: collect_non_flag_targets(&tail[1..]),
            }
        }
        // NPM-like scripts including yarn
        Some((tool, tail)) if (tool == "pnpm" || tool == "npm" || tool == "yarn") => {
            if let Some(cmd) = classify_npm_like(tool, tail, main_cmd) {
                cmd
            } else {
                ParsedCommand::Unknown {
                    cmd: main_cmd.to_vec(),
                }
            }
        }
        Some((head, tail)) if head == "ls" => {
            let path = tail
                .iter()
                .find(|p| !p.starts_with('-'))
                .map(|p| short_display_path(p));
            ParsedCommand::Ls {
                cmd: main_cmd.to_vec(),
                path,
            }
        }
        Some((head, tail)) if head == "rg" => {
            let args_no_connector = trim_at_connector(tail);
            let files_only = args_no_connector.iter().any(|a| a == "--files");
            let non_flags: Vec<&String> = args_no_connector
                .iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            let (query, path) = if files_only {
                let p = non_flags.first().map(|s| short_display_path(s));
                (None, p)
            } else {
                let q = non_flags.first().map(|s| short_display_path(s));
                let p = non_flags.get(1).map(|s| short_display_path(s));
                (q, p)
            };
            ParsedCommand::Search {
                cmd: main_cmd.to_vec(),
                query,
                path,
                files_only,
            }
        }
        Some((head, tail)) if head == "grep" => {
            let args_no_connector = trim_at_connector(tail);
            let non_flags: Vec<&String> = args_no_connector
                .iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            let query = non_flags.first().map(|s| short_display_path(s));
            let path = non_flags.get(1).map(|s| short_display_path(s));
            ParsedCommand::Search {
                cmd: main_cmd.to_vec(),
                query,
                path,
                files_only: false,
            }
        }
        Some((head, tail)) if head == "cat" => {
            // Support both `cat <file>` and `cat -- <file>` forms.
            let effective_tail: &[String] = if tail.first().map(|s| s.as_str()) == Some("--") {
                &tail[1..]
            } else {
                tail
            };
            if effective_tail.len() == 1 {
                let name = short_display_path(&effective_tail[0]);
                ParsedCommand::Read {
                    cmd: main_cmd.to_vec(),
                    name,
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: main_cmd.to_vec(),
                }
            }
        }
        Some((head, tail))
            if head == "head"
                && tail.len() >= 3
                && tail[0] == "-n"
                && tail[1].chars().all(|c| c.is_ascii_digit()) =>
        {
            let name = short_display_path(&tail[2]);
            ParsedCommand::Read {
                cmd: main_cmd.to_vec(),
                name,
            }
        }
        Some((head, tail))
            if head == "tail" && tail.len() >= 3 && tail[0] == "-n" && {
                let n = &tail[1];
                let s = n.strip_prefix('+').unwrap_or(n);
                !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
            } =>
        {
            let name = short_display_path(&tail[2]);
            ParsedCommand::Read {
                cmd: main_cmd.to_vec(),
                name,
            }
        }
        Some((head, tail))
            if head == "sed"
                && tail.len() >= 3
                && tail[0] == "-n"
                && is_valid_sed_n_arg(tail.get(1).map(|s| s.as_str())) =>
        {
            if let Some(path) = tail.get(2) {
                let name = short_display_path(path);
                ParsedCommand::Read {
                    cmd: main_cmd.to_vec(),
                    name,
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: main_cmd.to_vec(),
                }
            }
        }
        // Other commands
        _ => ParsedCommand::Unknown {
            cmd: main_cmd.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn vec_str(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn assert_parsed(args: &[String], expected: Vec<ParsedCommand>) {
        let out = parse_command(args);
        assert_eq!(out, expected);
    }

    #[test]
    fn git_status_is_unknown() {
        assert_parsed(
            &vec_str(&["git", "status"]),
            vec![ParsedCommand::Unknown {
                cmd: vec_str(&["git", "status"]),
            }],
        );
    }

    #[test]
    fn handles_complex_bash_command_head() {
        let inner =
            "rg --version && node -v && pnpm -v && rg --files | wc -l && rg --files | head -n 40";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![
                ParsedCommand::Unknown {
                    cmd: vec_str(&["head", "-n", "40"]),
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "--files"]),
                    query: None,
                    path: None,
                    files_only: true,
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "--files"]),
                    query: None,
                    path: None,
                    files_only: true,
                },
                ParsedCommand::Unknown {
                    cmd: vec_str(&["pnpm", "-v"]),
                },
                ParsedCommand::Unknown {
                    cmd: vec_str(&["node", "-v"]),
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "--version"]),
                    query: None,
                    path: None,
                    files_only: false,
                },
            ],
        );
    }

    #[test]
    fn supports_searching_for_navigate_to_route() {
        let inner = "rg -n \"navigate-to-route\" -S";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: shlex_split(inner).unwrap(),
                query: Some("navigate-to-route".to_string()),
                path: None,
                files_only: false,
            }],
        );
    }

    #[test]
    fn handles_complex_bash_command() {
        let inner = "rg -n \"BUG|FIXME|TODO|XXX|HACK\" -S | head -n 200";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![
                ParsedCommand::Unknown {
                    cmd: vec_str(&["head", "-n", "200"]),
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "-n", "BUG|FIXME|TODO|XXX|HACK", "-S"]),
                    query: Some("BUG|FIXME|TODO|XXX|HACK".to_string()),
                    path: None,
                    files_only: false,
                },
            ],
        );
    }

    #[test]
    fn supports_rg_files_with_path_and_pipe() {
        let inner = "rg --files webview/src | sed -n";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: vec_str(&["rg", "--files", "webview/src"]),
                query: None,
                path: Some("webview".to_string()),
                files_only: true,
            }],
        );
    }

    #[test]
    fn supports_rg_files_then_head() {
        let inner = "rg --files | head -n 50";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![
                ParsedCommand::Unknown {
                    cmd: vec_str(&["head", "-n", "50"]),
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "--files"]),
                    query: None,
                    path: None,
                    files_only: true,
                },
            ],
        );
    }

    #[test]
    fn supports_cat() {
        let inner = "cat webview/README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "README.md".to_string(),
            }],
        );
    }

    #[test]
    fn supports_ls_with_pipe() {
        let inner = "ls -la | sed -n '1,120p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Ls {
                cmd: shlex_split(inner).unwrap(),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_head_n() {
        let inner = "head -n 50 Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "Cargo.toml".to_string(),
            }],
        );
    }

    #[test]
    fn supports_cat_sed_n() {
        let inner = "cat tui/Cargo.toml | sed -n '1,200p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "Cargo.toml".to_string(),
            }],
        );
    }

    #[test]
    fn supports_tail_n_plus() {
        let inner = "tail -n +522 README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "README.md".to_string(),
            }],
        );
    }

    #[test]
    fn supports_tail_n_last_lines() {
        let inner = "tail -n 30 README.md";
        let out = parse_command(&vec_str(&["bash", "-lc", inner]));
        assert_eq!(
            out,
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "README.md".to_string(),
            }]
        );
    }

    #[test]
    fn supports_npm_run_build_is_unknown() {
        assert_parsed(
            &vec_str(&["npm", "run", "build"]),
            vec![ParsedCommand::Unknown {
                cmd: vec_str(&["npm", "run", "build"]),
            }],
        );
    }

    #[test]
    fn supports_npm_run_with_forwarded_args() {
        assert_parsed(
            &vec_str(&[
                "npm",
                "run",
                "lint",
                "--",
                "--max-warnings",
                "0",
                "--format",
                "json",
            ]),
            vec![ParsedCommand::Lint {
                cmd: vec_str(&[
                    "npm",
                    "run",
                    "lint",
                    "--",
                    "--max-warnings",
                    "0",
                    "--format",
                    "json",
                ]),
                tool: Some("npm-script:lint".to_string()),
                targets: None,
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_current_dir() {
        assert_parsed(
            &vec_str(&["grep", "-R", "CODEX_SANDBOX_ENV_VAR", "-n", "."]),
            vec![ParsedCommand::Search {
                cmd: vec_str(&["grep", "-R", "CODEX_SANDBOX_ENV_VAR", "-n", "."]),
                query: Some("CODEX_SANDBOX_ENV_VAR".to_string()),
                path: Some(".".to_string()),
                files_only: false,
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_specific_file() {
        assert_parsed(
            &vec_str(&[
                "grep",
                "-R",
                "CODEX_SANDBOX_ENV_VAR",
                "-n",
                "core/src/spawn.rs",
            ]),
            vec![ParsedCommand::Search {
                cmd: vec_str(&[
                    "grep",
                    "-R",
                    "CODEX_SANDBOX_ENV_VAR",
                    "-n",
                    "core/src/spawn.rs",
                ]),
                query: Some("CODEX_SANDBOX_ENV_VAR".to_string()),
                path: Some("spawn.rs".to_string()),
                files_only: false,
            }],
        );
    }

    #[test]
    fn supports_grep_weird_backtick_in_query() {
        assert_parsed(
            &vec_str(&["grep", "-R", "COD`EX_SANDBOX", "-n"]),
            vec![ParsedCommand::Search {
                cmd: vec_str(&["grep", "-R", "COD`EX_SANDBOX", "-n"]),
                query: Some("COD`EX_SANDBOX".to_string()),
                path: None,
                files_only: false,
            }],
        );
    }

    #[test]
    fn supports_cd_and_rg_files() {
        assert_parsed(
            &vec_str(&["cd", "codex-rs", "&&", "rg", "--files"]),
            vec![
                ParsedCommand::Unknown {
                    cmd: vec_str(&["cd", "codex-rs"]),
                },
                ParsedCommand::Search {
                    cmd: vec_str(&["rg", "--files"]),
                    query: None,
                    path: None,
                    files_only: true,
                },
            ],
        );
    }

    #[test]
    fn supports_cargo_fmt_and_test_with_config() {
        assert_parsed(
            &vec_str(&[
                "cargo",
                "fmt",
                "--",
                "--config",
                "imports_granularity=Item",
                "&&",
                "cargo",
                "test",
                "-p",
                "core",
                "--all-features",
            ]),
            vec![
                ParsedCommand::Format {
                    cmd: vec_str(&["cargo", "fmt", "--", "--config", "imports_granularity=Item"]),
                    tool: Some("cargo fmt".to_string()),
                    targets: None,
                },
                ParsedCommand::Test {
                    cmd: vec_str(&["cargo", "test", "-p", "core", "--all-features"]),
                    runner: Some("cargo".to_string()),
                    test_filter: Some(vec!["core".to_string()]),
                },
            ],
        );
    }

    #[test]
    fn recognizes_rustfmt_and_clippy() {
        assert_parsed(
            &vec_str(&["rustfmt", "src/main.rs"]),
            vec![ParsedCommand::Format {
                cmd: vec_str(&["rustfmt", "src/main.rs"]),
                tool: Some("rustfmt".to_string()),
                targets: Some(vec!["src/main.rs".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&[
                "cargo",
                "clippy",
                "-p",
                "core",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ]),
            vec![ParsedCommand::Lint {
                cmd: vec_str(&[
                    "cargo",
                    "clippy",
                    "-p",
                    "core",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ]),
                tool: Some("cargo clippy".to_string()),
                targets: None,
            }],
        );
    }

    #[test]
    fn recognizes_pytest_go_and_tools() {
        assert_parsed(
            &vec_str(&[
                "pytest",
                "-k",
                "Login and not slow",
                "tests/test_login.py::TestLogin::test_ok",
            ]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&[
                    "pytest",
                    "-k",
                    "Login and not slow",
                    "tests/test_login.py::TestLogin::test_ok",
                ]),
                runner: Some("pytest".to_string()),
                test_filter: Some(vec![
                    "Login and not slow".to_string(),
                    "tests/test_login.py::TestLogin::test_ok".to_string(),
                ]),
            }],
        );

        assert_parsed(
            &vec_str(&["go", "fmt", "./..."]),
            vec![ParsedCommand::Format {
                cmd: vec_str(&["go", "fmt", "./..."]),
                tool: Some("go fmt".to_string()),
                targets: Some(vec!["./...".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&["go", "test", "./pkg", "-run", "TestThing"]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&["go", "test", "./pkg", "-run", "TestThing"]),
                runner: Some("go".to_string()),
                test_filter: Some(vec!["TestThing".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&["eslint", ".", "--max-warnings", "0"]),
            vec![ParsedCommand::Lint {
                cmd: vec_str(&["eslint", ".", "--max-warnings", "0"]),
                tool: Some("eslint".to_string()),
                targets: Some(vec![".".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&["prettier", "-w", "."]),
            vec![ParsedCommand::Format {
                cmd: vec_str(&["prettier", "-w", "."]),
                tool: Some("prettier".to_string()),
                targets: Some(vec![".".to_string()]),
            }],
        );
    }

    #[test]
    fn recognizes_jest_and_vitest_filters() {
        assert_parsed(
            &vec_str(&["jest", "-t", "should work", "src/foo.test.ts"]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&["jest", "-t", "should work", "src/foo.test.ts"]),
                runner: Some("jest".to_string()),
                test_filter: Some(vec![
                    "should work".to_string(),
                    "src/foo.test.ts".to_string(),
                ]),
            }],
        );

        assert_parsed(
            &vec_str(&["vitest", "-t", "runs", "src/foo.test.tsx"]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&["vitest", "-t", "runs", "src/foo.test.tsx"]),
                runner: Some("vitest".to_string()),
                test_filter: Some(vec!["runs".to_string(), "src/foo.test.tsx".to_string()]),
            }],
        );
    }

    #[test]
    fn recognizes_npx_and_scripts() {
        assert_parsed(
            &vec_str(&["npx", "eslint", "src"]),
            vec![ParsedCommand::Lint {
                cmd: vec_str(&["npx", "eslint", "src"]),
                tool: Some("eslint".to_string()),
                targets: Some(vec!["src".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&["npx", "prettier", "-c", "."]),
            vec![ParsedCommand::Format {
                cmd: vec_str(&["npx", "prettier", "-c", "."]),
                tool: Some("prettier".to_string()),
                targets: Some(vec![".".to_string()]),
            }],
        );

        assert_parsed(
            &vec_str(&["pnpm", "run", "lint", "--", "--max-warnings", "0"]),
            vec![ParsedCommand::Lint {
                cmd: vec_str(&["pnpm", "run", "lint", "--", "--max-warnings", "0"]),
                tool: Some("pnpm-script:lint".to_string()),
                targets: None,
            }],
        );

        assert_parsed(
            &vec_str(&["npm", "test"]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&["npm", "test"]),
                runner: Some("npm-script".to_string()),
                test_filter: None,
            }],
        );

        assert_parsed(
            &vec_str(&["yarn", "test"]),
            vec![ParsedCommand::Test {
                cmd: vec_str(&["yarn", "test"]),
                runner: Some("yarn-script".to_string()),
                test_filter: None,
            }],
        );
    }

    // ---- is_small_formatting_command unit tests ----
    #[test]
    fn small_formatting_always_true_commands() {
        for cmd in [
            "wc", "tr", "cut", "sort", "uniq", "xargs", "tee", "column", "awk",
        ] {
            assert!(is_small_formatting_command(&vec_str(&[cmd])));
            assert!(is_small_formatting_command(&vec_str(&[cmd, "-x"])));
        }
    }

    #[test]
    fn head_behavior() {
        // No args -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["head"])));
        // Numeric count only -> not considered small formatting by implementation
        assert!(!is_small_formatting_command(&vec_str(&[
            "head", "-n", "40"
        ])));
        // With explicit file -> not small formatting
        assert!(!is_small_formatting_command(&vec_str(&[
            "head", "-n", "40", "file.txt"
        ])));
        // File only (no count) -> treated as small formatting by implementation
        assert!(is_small_formatting_command(&vec_str(&["head", "file.txt"])));
    }

    #[test]
    fn tail_behavior() {
        // No args -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["tail"])));
        // Numeric with plus offset -> not small formatting
        assert!(!is_small_formatting_command(&vec_str(&[
            "tail", "-n", "+10"
        ])));
        assert!(!is_small_formatting_command(&vec_str(&[
            "tail", "-n", "+10", "file.txt"
        ])));
        // Numeric count
        assert!(!is_small_formatting_command(&vec_str(&[
            "tail", "-n", "30"
        ])));
        assert!(!is_small_formatting_command(&vec_str(&[
            "tail", "-n", "30", "file.txt"
        ])));
        // File only -> small formatting by implementation
        assert!(is_small_formatting_command(&vec_str(&["tail", "file.txt"])));
    }

    #[test]
    fn sed_behavior() {
        // Plain sed -> small formatting
        assert!(is_small_formatting_command(&vec_str(&["sed"])));
        // sed -n <range> (no file) -> still small formatting
        assert!(is_small_formatting_command(&vec_str(&["sed", "-n", "10p"])));
        // Valid range with file -> not small formatting
        assert!(!is_small_formatting_command(&vec_str(&[
            "sed", "-n", "10p", "file.txt"
        ])));
        assert!(!is_small_formatting_command(&vec_str(&[
            "sed", "-n", "1,200p", "file.txt"
        ])));
        // Invalid ranges with file -> small formatting
        assert!(is_small_formatting_command(&vec_str(&[
            "sed", "-n", "p", "file.txt"
        ])));
        assert!(is_small_formatting_command(&vec_str(&[
            "sed", "-n", "+10p", "file.txt"
        ])));
    }

    #[test]
    fn empty_tokens_is_not_small() {
        let empty: Vec<String> = Vec::new();
        assert!(!is_small_formatting_command(&empty));
    }

    #[test]
    fn supports_nl_then_sed_reading() {
        let inner = "nl -ba core/src/parse_command.rs | sed -n '1200,1720p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "parse_command.rs".to_string(),
            }],
        );
    }

    #[test]
    fn filters_out_printf() {
        let inner =
            r#"printf "\n===== ansi-escape/Cargo.toml =====\n"; cat -- ansi-escape/Cargo.toml"#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: shlex_split(inner).unwrap(),
                name: "Cargo.toml".to_string(),
            }],
        );
    }

    #[test]
    fn drops_yes_in_pipelines() {
        // Inside bash -lc, `yes | rg --files` should focus on the primary command.
        let inner = "yes | rg --files";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: vec_str(&["rg", "--files"]),
                query: None,
                path: None,
                files_only: true,
            }],
        );
    }

    #[test]
    fn supports_sed_n_then_nl_as_search() {
        // Ensure `sed -n '<range>' <file> | nl -ba` is summarized as a search for that file.
        let args = vec_str(&[
            "sed",
            "-n",
            "260,640p",
            "exec/src/event_processor_with_human_output.rs",
            "|",
            "nl",
            "-ba",
        ]);
        assert_parsed(
            &args,
            vec![ParsedCommand::Search {
                cmd: vec_str(&[
                    "sed",
                    "-n",
                    "260,640p",
                    "exec/src/event_processor_with_human_output.rs",
                ]),
                query: None,
                path: Some("event_processor_with_human_output.rs".to_string()),
                files_only: false,
            }],
        );
    }
}
