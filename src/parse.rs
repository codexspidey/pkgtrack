/// Command prefixes to strip when extracting the "real" command.
const SKIP_PREFIXES: &[&str] = &[
    "sudo", "doas", "env", "time", "nohup", "exec", "command",
];

/// Extract all command words from a shell command line.
///
/// Handles:
/// - Leading VAR=value assignments
/// - Prefix commands (sudo, env, etc.)
/// - Chain operators (&&, ||, ;, |)
///
/// Returns a list of "first real command" for each segment.
pub fn extract_commands(line: &str) -> Vec<String> {
    let segments = split_segments(line);
    segments
        .into_iter()
        .filter_map(|seg| extract_first_command(&seg))
        .collect()
}

/// Split a command line on &&, ||, ;, | into segments.
fn split_segments(line: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_was_backslash = false;

    while let Some(ch) = chars.next() {
        if prev_was_backslash {
            current.push(ch);
            prev_was_backslash = false;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            prev_was_backslash = true;
            current.push(ch);
            continue;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
            continue;
        }

        if in_single_quote || in_double_quote {
            current.push(ch);
            continue;
        }

        match ch {
            ';' => {
                if !current.trim().is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next(); // skip second |
                }
                if !current.trim().is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next(); // skip second &
                    if !current.trim().is_empty() {
                        segments.push(std::mem::take(&mut current));
                    }
                } else {
                    // background &, treat as end of segment
                    if !current.trim().is_empty() {
                        segments.push(std::mem::take(&mut current));
                    }
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.trim().is_empty() {
        segments.push(current);
    }

    segments
}

/// Extract the first real command from a single segment.
/// Skips env var assignments and prefix commands.
fn extract_first_command(segment: &str) -> Option<String> {
    let tokens = tokenize(segment.trim());
    let mut i = 0;

    // Skip leading VAR=value assignments
    while i < tokens.len() {
        if is_env_assignment(&tokens[i]) {
            i += 1;
        } else {
            break;
        }
    }

    // Skip prefix commands
    while i < tokens.len() {
        let word = &tokens[i];
        if SKIP_PREFIXES.contains(&word.as_str()) {
            i += 1;
            // For `env`, also skip any VAR=value arguments and flags
            if word == "env" {
                while i < tokens.len() {
                    if is_env_assignment(&tokens[i]) || tokens[i].starts_with('-') {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            // For `sudo`, skip flags like -u, -i, etc.
            if word == "sudo" {
                while i < tokens.len() && tokens[i].starts_with('-') {
                    let flag = tokens[i].clone();
                    i += 1;
                    // Some flags take an argument (e.g., -u user)
                    if matches!(flag.as_str(), "-u" | "-g" | "-C") && i < tokens.len() {
                        i += 1;
                    }
                }
            }
            // For `command`, skip flags like -v, -V, -p
            if word == "command" {
                while i < tokens.len() && tokens[i].starts_with('-') {
                    i += 1;
                }
            }
        } else {
            break;
        }
    }

    if i < tokens.len() {
        let cmd = &tokens[i];
        // Extract basename from paths
        let name = if cmd.contains('/') {
            cmd.rsplit('/').next().unwrap_or(cmd)
        } else {
            cmd
        };
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    } else {
        None
    }
}

/// Simple whitespace tokenizer that respects quotes.
fn tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_was_backslash = false;

    for ch in s.chars() {
        if prev_was_backslash {
            current.push(ch);
            prev_was_backslash = false;
            continue;
        }

        if ch == '\\' && !in_single_quote {
            prev_was_backslash = true;
            continue;
        }

        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }

        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }

        if ch.is_whitespace() && !in_single_quote && !in_double_quote {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Check if a token looks like an env var assignment (KEY=value).
fn is_env_assignment(token: &str) -> bool {
    if let Some(eq_pos) = token.find('=') {
        if eq_pos == 0 {
            return false;
        }
        let key = &token[..eq_pos];
        // Key must be a valid env var name
        key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        assert_eq!(extract_commands("vim foo.rs"), vec!["vim"]);
    }

    #[test]
    fn test_sudo_prefix() {
        assert_eq!(extract_commands("sudo pacman -Syu"), vec!["pacman"]);
    }

    #[test]
    fn test_doas_prefix() {
        assert_eq!(extract_commands("doas pacman -S firefox"), vec!["pacman"]);
    }

    #[test]
    fn test_env_prefix() {
        assert_eq!(
            extract_commands("env DISPLAY=:0 firefox"),
            vec!["firefox"]
        );
    }

    #[test]
    fn test_leading_env_assignment() {
        assert_eq!(
            extract_commands("EDITOR=vim git commit"),
            vec!["git"]
        );
    }

    #[test]
    fn test_chain_operators() {
        assert_eq!(
            extract_commands("make && make install"),
            vec!["make", "make"]
        );
    }

    #[test]
    fn test_semicolons() {
        assert_eq!(
            extract_commands("cd foo; ls -la"),
            vec!["cd", "ls"]
        );
    }

    #[test]
    fn test_pipe() {
        assert_eq!(
            extract_commands("cat foo | grep bar | wc -l"),
            vec!["cat", "grep", "wc"]
        );
    }

    #[test]
    fn test_or_chain() {
        assert_eq!(
            extract_commands("test -f foo || touch foo"),
            vec!["test", "touch"]
        );
    }

    #[test]
    fn test_full_path() {
        assert_eq!(extract_commands("/usr/bin/vim"), vec!["vim"]);
    }

    #[test]
    fn test_multiple_prefixes() {
        assert_eq!(
            extract_commands("sudo env TERM=xterm nohup firefox"),
            vec!["firefox"]
        );
    }

    #[test]
    fn test_time_prefix() {
        assert_eq!(
            extract_commands("time cargo build"),
            vec!["cargo"]
        );
    }

    #[test]
    fn test_nohup_prefix() {
        assert_eq!(
            extract_commands("nohup discord &"),
            vec!["discord"]
        );
    }

    #[test]
    fn test_command_prefix() {
        assert_eq!(
            extract_commands("command -v gcc"),
            vec!["gcc"]
        );
    }

    #[test]
    fn test_mixed_chain_with_prefixes() {
        assert_eq!(
            extract_commands("sudo pacman -Syu && DISPLAY=:0 env GTK_THEME=dark firefox"),
            vec!["pacman", "firefox"]
        );
    }

    #[test]
    fn test_empty_line() {
        assert!(extract_commands("").is_empty());
        assert!(extract_commands("   ").is_empty());
    }

    #[test]
    fn test_only_env_assignment() {
        // FOO=bar with no command — the parser sees it as an env assignment
        // and returns nothing, OR treats it as a command. Both are acceptable.
        let result = extract_commands("FOO=bar");
        assert!(result.is_empty() || result == vec!["FOO=bar"]);
    }

    #[test]
    fn test_quoted_strings() {
        assert_eq!(
            extract_commands(r#"grep "hello world" foo.txt"#),
            vec!["grep"]
        );
    }

    #[test]
    fn test_sudo_with_flags() {
        assert_eq!(
            extract_commands("sudo -u nobody systemctl restart nginx"),
            vec!["systemctl"]
        );
    }

    #[test]
    fn test_exec_prefix() {
        assert_eq!(
            extract_commands("exec zsh"),
            vec!["zsh"]
        );
    }
}
