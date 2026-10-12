//! Building a Windows command line from arguments, portably (so it is tested everywhere).

/// Quotes one argument the way `CommandLineToArgvW` / the MSVC runtime will parse it back.
#[must_use]
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, and the quote itself is escaped.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Trailing backslashes precede the closing quote and must be doubled.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// `"exe" arg1 arg2 ...` ready for `CreateProcess`.
#[must_use]
pub fn command_line(exe: &str, args: &[String]) -> String {
    let mut line = quote_arg(exe);
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_untouched() {
        assert_eq!(quote_arg("--system"), "--system");
        assert_eq!(quote_arg(r"C:\x\y"), r"C:\x\y");
    }

    #[test]
    fn spaces_quotes_and_backslashes() {
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        // A trailing backslash inside quotes must not escape the closing quote.
        assert_eq!(quote_arg(r"C:\Program Files\"), r#""C:\Program Files\\""#);
        assert_eq!(quote_arg(r#"a\"b c"#), r#""a\\\"b c""#);
    }

    #[test]
    fn whole_command_line() {
        let line = command_line(
            r"C:\Program Files\Nexus\nexus-agent.exe",
            &["--system".into(), "--name".into(), "My PC".into()],
        );
        assert_eq!(
            line,
            r#""C:\Program Files\Nexus\nexus-agent.exe" --system --name "My PC""#
        );
    }
}
