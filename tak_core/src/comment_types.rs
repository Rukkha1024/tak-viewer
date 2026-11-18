//! Ordered Comment choices and their meanings, shared by the viewer and CLI.

use serde::{Deserialize, Serialize};

pub const MAX_TYPES: usize = 8;
pub const MAX_NAME: usize = 24;
pub const MAX_MEANING: usize = 240;
pub const DEFAULT_NAMES: &[&str] = &["File bad", "Range OK", "Range bad", "File OK"];
const DEFAULT_MEANINGS: &[&str] = &[
    "the whole file has a problem",
    "this frame range is normal",
    "this frame range is not normal",
    "the file has no problem",
];
const RETIRED_NAMES: &[&str] = &[
    "Jitter",
    "Jump",
    "Visual check",
    "Agree",
    "No problem",
    "Keep this only",
    "Needs visual check",
    "Marker swap",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub meaning: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TypeList(Vec<Definition>);

impl Default for TypeList {
    fn default() -> Self {
        Self(
            DEFAULT_NAMES
                .iter()
                .zip(DEFAULT_MEANINGS)
                .map(|(&name, &meaning)| Definition {
                    name: name.into(),
                    meaning: meaning.into(),
                })
                .collect(),
        )
    }
}

fn one_line(text: &str) -> bool {
    !text.contains(|c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

pub(crate) fn name_key(name: &str) -> String {
    name.to_uppercase()
}

impl TypeList {
    pub fn validate(mut rows: Vec<Definition>) -> Result<Self, String> {
        if rows.len() > MAX_TYPES {
            return Err(format!("Comment 종류는 {MAX_TYPES}개까지 만들 수 있어요."));
        }
        let mut names = std::collections::HashSet::new();
        for (i, row) in rows.iter_mut().enumerate() {
            if !one_line(&row.name) {
                return Err(format!("{}번째 이름은 한 줄로 써 주세요.", i + 1));
            }
            row.name = row.name.trim().to_owned();
            if row.name.is_empty() || row.name.chars().count() > MAX_NAME {
                return Err(format!(
                    "{}번째 이름은 1~{MAX_NAME}글자로 써 주세요.",
                    i + 1
                ));
            }
            let key = name_key(&row.name);
            if key == "NONE" {
                return Err("None은 종류를 지울 때 쓰는 이름이에요. 다른 이름을 써 주세요.".into());
            }
            if !names.insert(key) {
                return Err(format!("같은 이름이 있어요. ({})", row.name));
            }
            if !one_line(&row.meaning) || row.meaning.chars().count() > MAX_MEANING {
                return Err(format!(
                    "{}번째 뜻은 한 줄로 {MAX_MEANING}글자까지 써 주세요.",
                    i + 1
                ));
            }
        }
        Ok(Self(rows))
    }

    pub fn rows(&self) -> &[Definition] {
        &self.0
    }

    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.0.iter().map(|row| row.name.as_str())
    }

    pub fn rank(&self, name: &str) -> usize {
        if name.is_empty() {
            0
        } else {
            self.names()
                .position(|choice| choice == name)
                .map_or(self.0.len() + 1, |i| i + 1)
        }
    }

    pub fn next(&self, name: &str) -> &str {
        self.0.get(self.rank(name)).map_or("", |row| &row.name)
    }

    pub fn meaning(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.meaning.as_str())
    }

    /// Current names take priority over names accepted for older automation.
    pub(crate) fn accepted(&self, name: &str) -> Option<(&str, Option<&str>)> {
        let key = name_key(name);
        if let Some(row) = self.0.iter().find(|row| name_key(&row.name) == key) {
            return Some((&row.name, Some(&row.meaning)));
        }
        DEFAULT_NAMES
            .iter()
            .zip(DEFAULT_MEANINGS)
            .map(|(&name, &meaning)| (name, Some(meaning)))
            .chain(RETIRED_NAMES.iter().map(|&name| (name, None)))
            .find(|(name, _)| name_key(name) == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names_and_meanings_and_keeps_order() {
        let row = |name: &str, meaning: &str| Definition {
            name: name.into(),
            meaning: meaning.into(),
        };
        let list =
            TypeList::validate(vec![row("  Back  ", "정상, \"확인\""), row("First", "")]).unwrap();
        assert_eq!(list.names().collect::<Vec<_>>(), ["Back", "First"]);
        assert_eq!(
            list.accepted("back"),
            Some(("Back", Some("정상, \"확인\"")))
        );
        assert_eq!(list.next(""), "Back");
        assert_eq!(list.next("Back"), "First");
        assert_eq!(list.next("First"), "");
        assert_eq!(list.next("Old"), "");
        assert_eq!(list.rank("Old"), 3);
        assert_eq!(list.meaning("First"), Some(""));
        assert!(TypeList::validate(vec![]).is_ok());
        for bad in [
            vec![row("  ", "")],
            vec![row(" a\n", "")],
            vec![row("a", "x\ny")],
            vec![row("NONE", "")],
            vec![row("a", ""), row(" A ", "")],
            vec![row("É", ""), row("é", "")],
            vec![row(&"a".repeat(MAX_NAME + 1), "")],
            vec![row("a", &"가".repeat(MAX_MEANING + 1))],
            vec![row("a", ""); MAX_TYPES + 1],
        ] {
            assert!(TypeList::validate(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn defaults_and_old_cli_names_remain_available() {
        let defaults = TypeList::default();
        assert_eq!(defaults.names().collect::<Vec<_>>(), DEFAULT_NAMES);
        assert_eq!(
            defaults.meaning("Range OK"),
            Some("this frame range is normal")
        );
        let empty = TypeList::validate(vec![]).unwrap();
        assert_eq!(empty.accepted("RANGE OK"), defaults.accepted("Range OK"));
        for &name in RETIRED_NAMES {
            assert_eq!(empty.accepted(name), Some((name, None)));
        }
    }
}
