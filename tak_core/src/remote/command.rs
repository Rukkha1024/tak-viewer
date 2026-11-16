//! Command grammar shared by settings and SSH. Quotes group tokens; backslashes are literal.
use super::Result;

pub const PYTHON_NOT_FOUND: &str =
    "서버에서 Python 3을 찾지 못했어요. 서버 줄을 오른쪽 클릭해 'Python 명령'을 적어 주세요.";

pub fn python_tokens(command: &str, platform: &str) -> Result<Vec<String>> {
    if !matches!(platform, "linux" | "windows") {
        return Err("Remote platform must be linux or windows".into());
    }
    if command.len() > 8192 || command.chars().any(char::is_control) {
        return Err("Invalid remote Python command".into());
    }
    let (mut tokens, mut token, mut quote, mut started) = (Vec::new(), String::new(), None, false);
    for c in command.chars() {
        if quote == Some(c) {
            quote = None;
        } else if quote.is_some() {
            token.push(c);
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(c);
            started = true;
        }
    }
    if quote.is_some() {
        return Err("Unclosed quote in Python command".into());
    }
    if started {
        tokens.push(token);
    }
    if tokens.is_empty() || tokens.len() > 64 || tokens[0].is_empty() {
        return Err("Invalid remote Python command".into());
    }
    if platform == "windows"
        && tokens
            .iter()
            .any(|t| t.chars().any(|c| "&|<>^%!\"()".contains(c)))
    {
        return Err("Unsafe Windows Python command token".into());
    }
    Ok(tokens)
}

pub(super) fn candidates(
    platform: &str,
    explicit: &str,
    remembered: Option<&str>,
) -> Result<Vec<String>> {
    let defaults: &[&str] = match platform {
        "windows" => &["py -3", r"C:\Python313\python.exe", "python"],
        "linux" => &["python3", "python"],
        _ => return Err("Remote platform must be linux or windows".into()),
    };
    if !explicit.trim().is_empty() {
        python_tokens(explicit, platform)?;
        return Ok(vec![explicit.to_owned()]);
    }
    let mut commands = Vec::new();
    if let Some(command) = remembered.filter(|s| !s.trim().is_empty()) {
        python_tokens(command, platform)?;
        commands.push(command.to_owned());
    }
    for command in defaults {
        if !commands.iter().any(|c| c == command) {
            commands.push((*command).to_owned());
        }
    }
    Ok(commands)
}

pub(super) fn discover<T>(
    commands: Vec<String>,
    cancel: &super::CancelToken,
    mut attempt: impl FnMut(&str) -> Result<T>,
) -> Result<(String, T)> {
    let mut failures = Vec::new();
    for command in commands {
        if cancel.is_cancelled() {
            return Err("SSH request cancelled".into());
        }
        match attempt(&command) {
            Ok(value) => return Ok((command, value)),
            Err(error) => failures.push(format!("{command}: {error}")),
        }
    }
    if cancel.is_cancelled() {
        return Err("SSH request cancelled".into());
    }
    Err(format!("{PYTHON_NOT_FOUND}\n{}", failures.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_keep_paths_spaces_equals_and_arguments() {
        assert_eq!(
            python_tokens(
                r#""C:\Program Files\Python\python.exe" -I -X utf8"#,
                "windows"
            )
            .unwrap(),
            [r"C:\Program Files\Python\python.exe", "-I", "-X", "utf8"]
        );
        assert_eq!(
            python_tokens(
                r"codex sandbox -c sandbox_mode=read-only -- C:\Python313\python.exe",
                "windows"
            )
            .unwrap()
            .len(),
            6
        );
        assert_eq!(
            python_tokens("'/opt/my python' -X 'a=b'", "linux").unwrap(),
            ["/opt/my python", "-X", "a=b"]
        );
        for command in ["", "''", "py\n-3", "\"unterminated"] {
            assert!(python_tokens(command, "linux").is_err());
        }
        for c in "&|<>^%!\"()".chars() {
            let command = format!("'py{c}bad' -3");
            assert!(python_tokens(&command, "windows").is_err(), "{command}");
        }
    }
    #[test]
    fn discovery_order_remembering_and_empty_auto() {
        assert_eq!(
            candidates("windows", "", None).unwrap(),
            ["py -3", r"C:\Python313\python.exe", "python"]
        );
        assert_eq!(
            candidates("linux", "", None).unwrap(),
            ["python3", "python"]
        );
        assert_eq!(
            candidates("windows", "  ", Some("custom -3")).unwrap()[0],
            "custom -3"
        );
        assert_eq!(
            candidates("windows", "explicit -I", Some("custom")).unwrap(),
            ["explicit -I"]
        );
    }
    #[test]
    fn stub_or_failed_handshake_is_never_selected() {
        let mut tried = Vec::new();
        let found = discover(
            candidates("windows", "", None).unwrap(),
            &super::super::CancelToken::default(),
            |c| {
                tried.push(c.to_owned());
                if c == "py -3" {
                    Err("Store stub: no valid helper handshake".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
        assert_eq!(found.0, r"C:\Python313\python.exe");
        assert_eq!(tried.len(), 2);
        assert!(discover::<()>(
            vec!["python".into()],
            &super::super::CancelToken::default(),
            |_| Err("stub".into())
        )
        .unwrap_err()
        .contains(PYTHON_NOT_FOUND));
    }
}
