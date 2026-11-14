//! Native folder opening and user-visible shortcut names.

use std::borrow::Cow;
use std::path::Path;
use std::process::Command;

pub fn shortcut_text(text: &str) -> Cow<'_, str> {
    #[cfg(target_os = "macos")]
    if text.contains("Ctrl") || text.contains("Alt") {
        return Cow::Owned(text.replace("Ctrl", "Cmd").replace("Alt", "Option"));
    }
    Cow::Borrowed(text)
}

fn folder_command(path: &Path) -> Command {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(windows)]
    let mut command = Command::new("explorer");
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = Command::new("xdg-open");
    command.arg(path);
    command
}

pub fn open_folder(path: &Path) -> std::io::Result<()> {
    // Canonicalizing makes leading dashes literal absolute paths on Unix. Keep Windows
    // paths as supplied: canonicalize adds a verbatim prefix that Explorer may not accept.
    #[cfg(not(windows))]
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "폴더가 아니에요."));
    }
    #[cfg(windows)]
    {
        // Explorer can return a nonzero exit code when reusing an existing window.
        folder_command(&path).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let output = folder_command(&path).output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("폴더를 열 수 없어요 ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_names_match_the_platform() {
        let label = "Ctrl+Shift+S · Alt+클릭";
        #[cfg(target_os = "macos")]
        assert_eq!(shortcut_text(label), "Cmd+Shift+S · Option+클릭");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(shortcut_text(label), label);
        assert!(matches!(shortcut_text("Shift+F"), Cow::Borrowed(_)));
    }

    #[test]
    fn folder_path_is_one_literal_argument() {
        let path = Path::new("/tmp/한글 폴더;$(touch should-not-exist)");
        let command = folder_command(path);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
        #[cfg(target_os = "macos")]
        assert_eq!(command.get_program(), "/usr/bin/open");
        #[cfg(windows)]
        assert_eq!(command.get_program(), "explorer");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nonexistent_folder_reports_failure() {
        let path = std::env::temp_dir().join(format!("tak_missing_folder_{}", std::process::id()));
        assert!(!path.exists());
        assert!(open_folder(&path).is_err());
    }
}
