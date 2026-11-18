use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum IncrementMode {
    Stay,
    #[default]
    Next,
    NextMissing,
}

impl IncrementMode {
    pub(crate) const ALL: [Self; 3] = [Self::Stay, Self::Next, Self::NextMissing];

    pub(crate) fn caption(self) -> &'static str {
        match self {
            Self::Stay => "Do Not Increment",
            Self::Next => "Go To Next Label",
            Self::NextMissing => "Go To Next Unlabeled Marker",
        }
    }
}

impl Serialize for IncrementMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Stay => "stay",
            Self::Next => "next",
            Self::NextMissing => "next_missing",
        })
    }
}

impl<'de> Deserialize<'de> for IncrementMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ModeVisitor;

        impl Visitor<'_> for ModeVisitor {
            type Value = IncrementMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a label increment mode or a legacy bool")
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(if value { IncrementMode::Next } else { IncrementMode::Stay })
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "stay" => Ok(IncrementMode::Stay),
                    "next" => Ok(IncrementMode::Next),
                    "next_missing" => Ok(IncrementMode::NextMissing),
                    _ => Err(E::unknown_variant(value, &["stay", "next", "next_missing"])),
                }
            }
        }

        deserializer.deserialize_any(ModeVisitor)
    }
}
