//! Pure helpers: command-line quoting and the "use a list file" decision.

/// More paths than this go through a `--convert-list` file.
pub const MAX_DIRECT_PATHS: usize = 20;

/// Quote one argument so that `CommandLineToArgvW` parses it back unchanged.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"')) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Double the backslashes before a quote, then escape the quote.
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
    // Backslashes before the closing quote must be doubled.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// True if the paths must be passed in a list file.
pub fn needs_list_file(count: usize) -> bool {
    count > MAX_DIRECT_PATHS
}

/// Command line for the direct form: `"<exe>" --convert <paths...>`.
pub fn convert_command_line(exe: &str, paths: &[String]) -> String {
    let mut s = quote_arg(exe);
    s.push_str(" --convert");
    for p in paths {
        s.push(' ');
        s.push_str(&quote_arg(p));
    }
    s
}

/// Command line for the list form: `"<exe>" --convert-list <file>`.
pub fn convert_list_command_line(exe: &str, list_file: &str) -> String {
    format!("{} --convert-list {}", quote_arg(exe), quote_arg(list_file))
}

/// Text of the list file: one path per line.
pub fn list_file_text(paths: &[String]) -> String {
    let mut s = paths.join("\r\n");
    s.push_str("\r\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_unchanged() {
        assert_eq!(quote_arg(r"C:\a\b.png"), r"C:\a\b.png");
    }

    #[test]
    fn spaces_are_quoted() {
        assert_eq!(quote_arg(r"C:\my dir\b.png"), r#""C:\my dir\b.png""#);
    }

    #[test]
    fn trailing_backslash_is_doubled() {
        assert_eq!(quote_arg(r"C:\my dir\"), r#""C:\my dir\\""#);
    }

    #[test]
    fn quotes_are_escaped() {
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn empty_is_quoted() {
        assert_eq!(quote_arg(""), "\"\"");
    }

    #[test]
    fn list_decision() {
        assert!(!needs_list_file(0));
        assert!(!needs_list_file(20));
        assert!(needs_list_file(21));
    }

    #[test]
    fn command_lines() {
        let p = vec![r"C:\a b\x.png".to_string(), r"C:\y.png".to_string()];
        assert_eq!(
            convert_command_line(r"C:\Z\zest.exe", &p),
            r#"C:\Z\zest.exe --convert "C:\a b\x.png" C:\y.png"#
        );
        assert_eq!(
            convert_list_command_line(r"C:\Z a\zest.exe", r"C:\t\l.txt"),
            r#""C:\Z a\zest.exe" --convert-list C:\t\l.txt"#
        );
    }

    #[test]
    fn list_text() {
        assert_eq!(list_file_text(&["a".into(), "b".into()]), "a\r\nb\r\n");
    }
}
