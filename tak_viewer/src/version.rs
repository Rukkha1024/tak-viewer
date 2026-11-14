//! The package version comes from the workspace manifest and is shared by both crates.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Window title: program name + version, then the open file if any.
pub fn window_title(file: Option<&str>) -> String {
    match file {
        Some(f) => format!("TAK 뷰어 v{VERSION} — {f}"),
        None => format!("TAK 뷰어 v{VERSION}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_shows_version_and_file() {
        assert_eq!(window_title(None), format!("TAK 뷰어 v{VERSION}"));
        assert_eq!(window_title(Some("a.tak")), format!("TAK 뷰어 v{VERSION} — a.tak"));
    }

    /// Raising the version without writing the change log (or the reverse) fails here.
    #[test]
    fn changelog_top_entry_is_this_version() {
        let log = include_str!("../../CHANGELOG.md");
        let top = log.lines().find(|l| l.starts_with("## v")).expect("CHANGELOG에 ## v 제목이 없어요");
        let want = format!("## v{VERSION} ");
        assert!(
            top.starts_with(&want) || top == want.trim_end(),
            "CHANGELOG 맨 위 버전({top})이 Cargo.toml 버전(v{VERSION})과 달라요"
        );
    }
}
