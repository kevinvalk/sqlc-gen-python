/// A token with the depth of parentheses and brackets it stands at.
pub(super) type Leveled<'t> = (usize, &'t str);

/// A parenthesis or a bracket itself stands at the depth outside it.
pub(super) fn leveled(tokens: &[String]) -> Vec<Leveled<'_>> {
    let mut depth = 0usize;
    (tokens.iter())
        .map(|token| {
            if matches!(token.as_str(), ")" | "]") {
                depth = depth.saturating_sub(1);
            }
            let level = depth;
            if matches!(token.as_str(), "(" | "[") {
                depth += 1;
            }
            (level, token.as_str())
        })
        .collect()
}

/// Tokens from inside parentheses, leveled again so that their own top is 0.
pub(super) fn lowered<'t>(tokens: &[Leveled<'t>]) -> Vec<Leveled<'t>> {
    let depth = tokens.first().map_or(0, |&(d, _)| d);
    (tokens.iter()).map(|&(d, t)| (d.saturating_sub(depth), t)).collect()
}

/// Whether `token` is one of `words`, ignoring case.
pub(super) fn is(token: &str, words: &[&str]) -> bool {
    words.iter().any(|word| token.eq_ignore_ascii_case(word))
}

/// Whether the token at `at` is a name whatever it spells (`k.group`, `AS order`), though not after a number's dot
/// (`1. GROUP BY`).
pub(super) fn is_name_at(tokens: &[Leveled], at: usize) -> bool {
    match &tokens[..at] {
        [.., (_, number), (_, ".")] => !number.starts_with(|c: char| c.is_ascii_digit()),
        [.., (_, before)] => is(before, &[".", "as"]),
        [] => false,
    }
}

/// Where the first of `words` stands at the top level of `tokens` as a keyword: not as a name, and not as the `FROM` of
/// `IS DISTINCT FROM` or the `GROUP` of `WITHIN GROUP`.
pub(super) fn top(tokens: &[Leveled], words: &[&str]) -> Option<usize> {
    let mut before = "";
    (0..tokens.len()).find(|&at| {
        let (depth, token) = tokens[at];
        if depth != 0 {
            return false;
        }
        let name = is_name_at(tokens, at);
        let operator = is(before, &["distinct", "within"]) && is(token, &["from", "group"]);
        before = match name {
            true => "",
            false => token,
        };
        is(token, words) && !name && !operator
    })
}

/// What the parentheses `tokens` open with hold, and what follows them.
pub(super) fn parenthesised<'b, 't>(tokens: &'b [Leveled<'t>]) -> Option<(&'b [Leveled<'t>], &'b [Leveled<'t>])> {
    let [(depth, "("), rest @ ..] = tokens else {
        return None;
    };
    let close = rest.iter().position(|&(d, t)| d == *depth && t == ")")?;
    Some((&rest[..close], &rest[close + 1..]))
}

/// The words of the dotted name that `item` is, where it is a bare column with at most an alias.
pub(super) fn column<'t>(item: &[Leveled<'t>]) -> Option<Vec<&'t str>> {
    let (mut words, mut rest) = (Vec::new(), item);
    while let [(_, word), after @ ..] = rest
        && is_word(word)
    {
        words.push(*word);
        rest = after;
        match after {
            [(_, "."), more @ ..] => rest = more,
            _ => break,
        }
    }
    (!words.is_empty() && is_alias(rest)).then_some(words)
}

/// Whether `tokens` are at most an alias: nothing, `alias`, or `AS alias`.
pub(super) fn is_alias(tokens: &[Leveled]) -> bool {
    match tokens {
        [] => true,
        [(_, alias)] => is_word(alias),
        [(_, r#as), (_, alias)] => is(r#as, &["as"]) && is_word(alias),
        _ => false,
    }
}

/// Whether `token` is a name or an alias, quoted or not.
pub(super) fn is_word(token: &str) -> bool {
    token.starts_with(|c: char| c.is_alphabetic() || "_\"`[".contains(c))
}
