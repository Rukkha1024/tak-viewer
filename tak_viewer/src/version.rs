//! Program version. The number lives in one place: `[workspace.package] version` in
//! `apps/Cargo.toml` (tak_core and tak_viewer share it). Each released version has a
//! `## vX.Y.Z` heading at the top of `apps/CHANGELOG.md` and a git tag `vX.Y.Z` in the apps repo.

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
