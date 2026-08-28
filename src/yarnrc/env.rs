//! Environment variable interpolation for `.yarnrc.yml` values.
//!
//! Ports `replaceEnvVariables` from yarn berry's
//! `packages/yarnpkg-core/sources/miscUtils.ts`, so a `.yarnrc.yml` that yarn accepts
//! resolves to the same string here. That covers the three substitution forms documented
//! at <https://yarnpkg.com/configuration/yarnrc>, fallbacks that themselves contain
//! substitutions, and the `\\`, `\$` and `\}` escapes.

use anyhow::bail;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Operator {
    /// `${NAME}`. The variable has to be set, an empty value is still a value.
    Required,
    /// `${NAME-fallback}`. Falls back only when the variable is unset.
    Unset,
    /// `${NAME:-fallback}`. Falls back when the variable is unset or empty.
    UnsetOrEmpty,
}

#[derive(Debug)]
enum Token<'a> {
    /// `\\`, `\$` or `\}`, carrying the escaped character.
    Escaped(char),
    Variable {
        name: &'a str,
        operator: Operator,
    },
    /// A `${` that does not open one of the substitution forms.
    Unknown,
    CloseBrace,
}

/// Walks a `.yarnrc.yml` value, yielding the pieces that interpolation acts on and
/// skipping over the literal text between them.
struct Scanner<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    /// Returns the next token together with the byte range it spans.
    fn next_token(&mut self) -> Option<(usize, usize, Token<'a>)> {
        let bytes = self.input.as_bytes();
        let mut i = self.pos;

        while i < bytes.len() {
            match bytes[i] {
                b'\\' if matches!(bytes.get(i + 1), Some(b'\\' | b'$' | b'}')) => {
                    self.pos = i + 2;
                    return Some((i, i + 2, Token::Escaped(bytes[i + 1] as char)));
                }
                b'$' if bytes.get(i + 1) == Some(&b'{') => {
                    self.pos = i + 2;
                    return Some(match self.parse_variable(i) {
                        Some(token) => token,
                        None => (i, i + 2, Token::Unknown),
                    });
                }
                b'}' => {
                    self.pos = i + 1;
                    return Some((i, i + 1, Token::CloseBrace));
                }
                _ => i += 1,
            }
        }

        self.pos = bytes.len();
        None
    }

    /// Parses a substitution opening at `start`, where `start..start + 2` is `${`.
    /// Sets `self.pos` past what it consumed and returns `None` if the name or the
    /// operator does not parse, leaving the caller to report a `${` it cannot read.
    fn parse_variable(&mut self, start: usize) -> Option<(usize, usize, Token<'a>)> {
        let bytes = self.input.as_bytes();
        let name_start = start + 2;

        if !bytes.get(name_start)?.is_ascii_alphabetic() {
            return None;
        }
        let mut end = name_start + 1;
        while matches!(bytes.get(end), Some(c) if c.is_ascii_alphanumeric() || *c == b'_') {
            end += 1;
        }
        let name = &self.input[name_start..end];

        // The name is matched greedily and every operator starts on a character that
        // cannot be part of a name, so a shorter name would never parse either.
        let (operator, consumed_to) = match bytes.get(end) {
            Some(b':') if bytes.get(end + 1) == Some(&b'-') => (Operator::UnsetOrEmpty, end + 2),
            Some(b'-') => (Operator::Unset, end + 1),
            // The closing brace is looked at but left for the caller to consume, so that
            // it decrements the nesting depth like any other closing brace.
            Some(b'}') => (Operator::Required, end),
            _ => return None,
        };

        self.pos = consumed_to;
        Some((start, consumed_to, Token::Variable { name, operator }))
    }
}

/// Resolves every substitution in `input`, looking variables up with `lookup`.
///
/// Errors when `${NAME}` names a variable that is not set, and when the substitution
/// syntax itself does not parse.
pub fn interpolate(input: &str, lookup: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let mut output = String::new();
    let mut current = 0;
    // How many substitutions are open around the position being read. Fallback text sits
    // one level deeper than the value it belongs to, so this is what tells a closing brace
    // that ends a substitution apart from one that is just a character in the value.
    let mut depth: isize = 0;
    let mut scanner = Scanner::new(input);

    while let Some((start, end, token)) = scanner.next_token() {
        output.push_str(&input[current..start]);
        current = end;

        match token {
            Token::Escaped(c) => output.push(c),
            Token::Variable { name, operator } => {
                let value = lookup(name);
                depth += 1;

                let resolved = match (operator, value.as_deref()) {
                    (Operator::UnsetOrEmpty, Some("")) => None,
                    (_, value) => value,
                };

                match resolved {
                    Some(value) => {
                        output.push_str(value);
                        // The value won, so drop the fallback that follows it.
                        current = skip_fallback(&mut scanner, &mut depth, input.len());
                    }
                    // Nothing to fall back to.
                    None if operator == Operator::Required => {
                        bail!("environment variable not found: {name}")
                    }
                    // Leave the fallback for the loop to read as ordinary text.
                    None => {}
                }
            }
            Token::CloseBrace => {
                if depth == 0 {
                    output.push('}');
                } else {
                    depth -= 1;
                }
            }
            Token::Unknown => {
                bail!("invalid environment variable substitution syntax: {input}")
            }
        }
    }

    if depth > 0 {
        bail!("incomplete environment variable substitution: {input}");
    }

    output.push_str(&input[current..]);
    Ok(output)
}

/// Consumes the fallback of the substitution that was just resolved, and returns the byte
/// offset just past its closing brace. Substitutions nested in the fallback are skipped
/// whole rather than resolved, so an unset variable in a discarded fallback is not an error.
fn skip_fallback(scanner: &mut Scanner, depth: &mut isize, input_len: usize) -> usize {
    let limit = *depth;

    while let Some((_, end, token)) = scanner.next_token() {
        match token {
            Token::Variable { .. } => *depth += 1,
            Token::CloseBrace => {
                *depth -= 1;
                if *depth < limit {
                    return end;
                }
            }
            _ => {}
        }
    }

    input_len
}

/// Resolves every substitution in `input` against the process environment.
pub fn interpolate_from_env(input: &str) -> anyhow::Result<String> {
    interpolate(input, &|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    fn resolve(input: &str, pairs: &[(&str, &str)]) -> anyhow::Result<String> {
        interpolate(input, &env(pairs))
    }

    #[test]
    fn passes_through_text_without_substitutions() {
        assert_eq!(resolve("plain token", &[]).unwrap(), "plain token");
        assert_eq!(resolve("", &[]).unwrap(), "");
    }

    #[test]
    fn substitutes_a_set_variable() {
        assert_eq!(resolve("${TOKEN}", &[("TOKEN", "abc")]).unwrap(), "abc");
        assert_eq!(
            resolve("Bearer ${TOKEN}!", &[("TOKEN", "abc")]).unwrap(),
            "Bearer abc!"
        );
        assert_eq!(
            resolve("${A}${B}", &[("A", "1"), ("B", "2")]).unwrap(),
            "12"
        );
    }

    #[test]
    fn required_form_accepts_an_empty_value_but_not_a_missing_one() {
        assert_eq!(resolve("${TOKEN}", &[("TOKEN", "")]).unwrap(), "");

        let err = resolve("${TOKEN}", &[]).unwrap_err().to_string();
        assert_eq!(err, "environment variable not found: TOKEN");
    }

    #[test]
    fn dash_falls_back_only_when_unset() {
        assert_eq!(resolve("${TOKEN-fb}", &[("TOKEN", "abc")]).unwrap(), "abc");
        // Set but empty still counts as set.
        assert_eq!(resolve("${TOKEN-fb}", &[("TOKEN", "")]).unwrap(), "");
        assert_eq!(resolve("${TOKEN-fb}", &[]).unwrap(), "fb");
        assert_eq!(resolve("${TOKEN-}", &[]).unwrap(), "");
    }

    #[test]
    fn colon_dash_falls_back_when_unset_or_empty() {
        assert_eq!(resolve("${TOKEN:-fb}", &[("TOKEN", "abc")]).unwrap(), "abc");
        assert_eq!(resolve("${TOKEN:-fb}", &[("TOKEN", "")]).unwrap(), "fb");
        assert_eq!(resolve("${TOKEN:-fb}", &[]).unwrap(), "fb");
    }

    #[test]
    fn resolves_substitutions_inside_a_used_fallback() {
        assert_eq!(resolve("${A:-${B}}", &[("B", "from-b")]).unwrap(), "from-b");
        assert_eq!(resolve("${A:-${B:-deep}}", &[]).unwrap(), "deep");
        assert_eq!(
            resolve("prefix-${A:-${B}}-suffix", &[("B", "b")]).unwrap(),
            "prefix-b-suffix"
        );
    }

    #[test]
    fn skips_a_fallback_the_value_made_redundant() {
        // B is never read, so its being unset is not an error.
        assert_eq!(resolve("${A:-${B}}", &[("A", "from-a")]).unwrap(), "from-a");
        assert_eq!(
            resolve("${A:-${B}}after", &[("A", "from-a")]).unwrap(),
            "from-aafter"
        );
        assert_eq!(
            resolve("${A:-plain text}", &[("A", "from-a")]).unwrap(),
            "from-a"
        );
    }

    #[test]
    fn ends_a_skipped_fallback_on_the_first_unpaired_closing_brace() {
        // Skipping counts closing braces and nothing else, so an opening brace that is not
        // part of a substitution does not hold the fallback open. Kept because this is what
        // yarn resolves such a value to, not because the value is worth writing.
        assert_eq!(resolve("${A:-x{y}z}", &[("A", "v")]).unwrap(), "vz}");
    }

    #[test]
    fn honours_escapes() {
        assert_eq!(resolve(r"\${TOKEN}", &[]).unwrap(), "${TOKEN}");
        assert_eq!(resolve(r"a\\b", &[]).unwrap(), r"a\b");
        assert_eq!(resolve(r"a\}b", &[]).unwrap(), "a}b");
        // A backslash that escapes nothing stays put.
        assert_eq!(resolve(r"a\nb", &[]).unwrap(), r"a\nb");
    }

    #[test]
    fn treats_an_unmatched_closing_brace_as_text() {
        assert_eq!(resolve("a}b", &[]).unwrap(), "a}b");
        assert_eq!(resolve("${A}}", &[("A", "x")]).unwrap(), "x}");
    }

    #[test]
    fn rejects_syntax_it_cannot_read() {
        for input in ["${}", "${1ABC}", "${A B}", "${A+b}"] {
            let err = resolve(input, &[]).unwrap_err().to_string();
            assert_eq!(
                err,
                format!("invalid environment variable substitution syntax: {input}")
            );
        }
    }

    #[test]
    fn rejects_a_substitution_that_is_never_closed() {
        let err = resolve("${A:-fb", &[]).unwrap_err().to_string();
        assert_eq!(err, "incomplete environment variable substitution: ${A:-fb");
    }
}
