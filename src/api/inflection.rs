//! Singularising table names exactly as sqlc does, so a model class has the
//! name sqlc's Go codegen gives the same table's struct (`media` is `Medium`).
//!
//! This ports sqlc's `internal/inflection`: its corrections, then the rules of
//! `github.com/jinzhu/inflection` v1.0.0 in the order that library tries them,
//! quirks included (`leaves` is `leafe`). `tests/sqlc/endtoend/inflection.tsv`,
//! captured from sqlc's own code, holds the port to it.

use std::sync::LazyLock;

use regex::Regex;

#[cfg(test)]
#[path = "inflection_spec.rs"]
mod spec;

const UNCOUNTABLE: &[&str] = &[
    "equipment",
    "information",
    "rice",
    "money",
    "species",
    "series",
    "fish",
    "sheep",
    "jeans",
    "police",
];

/// `(singular, plural)`.
const IRREGULAR: &[(&str, &str)] = &[
    ("person", "people"),
    ("man", "men"),
    ("child", "children"),
    ("sex", "sexes"),
    ("move", "moves"),
    ("mombie", "mombies"),
];

/// `(find, replace)`, tried last to first.
const SINGULAR: &[(&str, &str)] = &[
    ("s$", ""),
    ("(ss)$", "${1}"),
    ("(n)ews$", "${1}ews"),
    ("([ti])a$", "${1}um"),
    (
        "((a)naly|(b)a|(d)iagno|(p)arenthe|(p)rogno|(s)ynop|(t)he)(sis|ses)$",
        "${1}sis",
    ),
    ("(^analy)(sis|ses)$", "${1}sis"),
    ("([^f])ves$", "${1}fe"),
    ("(hive)s$", "${1}"),
    ("(tive)s$", "${1}"),
    ("([lr])ves$", "${1}f"),
    ("([^aeiouy]|qu)ies$", "${1}y"),
    ("(s)eries$", "${1}eries"),
    ("(m)ovies$", "${1}ovie"),
    ("(c)ookies$", "${1}ookie"),
    ("(x|ch|ss|sh)es$", "${1}"),
    ("^(m|l)ice$", "${1}ouse"),
    ("(bus)(es)?$", "${1}"),
    ("(o)es$", "${1}"),
    ("(shoe)s$", "${1}"),
    ("(cris|test)(is|es)$", "${1}is"),
    ("^(a)x[ie]s$", "${1}xis"),
    ("(octop|vir)(us|i)$", "${1}us"),
    ("(alias|status)(es)?$", "${1}"),
    ("^(ox)en", "${1}"),
    ("(vert|ind)ices$", "${1}ex"),
    ("(matr)ices$", "${1}ix"),
    ("(quiz)zes$", "${1}"),
    ("(database)s$", "${1}"),
];

/// jinzhu's rules as it compiles them: uncountables, then irregulars and
/// regular rules each in an upper-case, an as-written and a case-insensitive
/// form. The first that matches decides.
static RULES: LazyLock<Vec<(Regex, String)>> = LazyLock::new(|| {
    let title = |word: &str| word[..1].to_uppercase() + &word[1..];
    let uncountable = (UNCOUNTABLE.iter()).map(|word| (format!("^(?i)({word})$"), "${1}".into()));
    let irregular = IRREGULAR.iter().flat_map(|&(singular, plural)| {
        [
            (plural.to_uppercase() + "$", singular.to_uppercase()),
            (title(plural) + "$", title(singular)),
            (plural.to_owned() + "$", singular.to_owned()),
        ]
    });
    let regular = SINGULAR.iter().rev().flat_map(|&(find, replace)| {
        [
            (find.to_uppercase(), replace.to_uppercase()),
            (find.to_owned(), replace.to_owned()),
            (format!("(?i){find}"), replace.to_owned()),
        ]
    });
    (uncountable.chain(irregular).chain(regular))
        .map(|(find, replace)| (Regex::new(&find).expect("a valid rule"), replace))
        .collect()
});

/// The singular of a table name, as sqlc's `inflection.Singular` gives it.
pub fn singular(name: &str) -> String {
    // sqlc's corrections of jinzhu, each for an issue filed against sqlc.
    match name.to_lowercase().as_str() {
        "campus" | "meta" => return name.into(),
        "calories" => return "calorie".into(),
        "waves" => return "wave".into(),
        "metadata" => return "metadata".into(),
        _ => {}
    }
    (RULES.iter())
        .find(|(rule, _)| rule.is_match(name))
        .map_or_else(
            || name.into(),
            |(rule, replace)| rule.replace_all(name, replace.as_str()).into_owned(),
        )
}
