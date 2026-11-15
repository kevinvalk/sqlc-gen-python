//! Specification for what the plugin prints when it fails.

use super::*;

#[test]
fn an_error_reaches_the_terminal_without_control_characters() {
    assert_eq!(
        printable("query \"a\u{1b}[2Jb\":\n\tno\r\u{9b}"),
        "query \"a\\u{1b}[2Jb\":\n\\tno\\r\\u{9b}"
    );
    assert_eq!(printable("naïve \"名前\""), "naïve \"名前\"");
}
