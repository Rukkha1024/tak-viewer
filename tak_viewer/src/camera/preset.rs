use tak_core::geom::{v3, Vec3};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    Perspective,
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
}

impl Preset {
    pub const ALL: [Self; 7] = [
        Self::Perspective, Self::Front, Self::Back, Self::Left, Self::Right, Self::Top, Self::Bottom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Front => "Front",
            Self::Back => "Back",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.label().eq_ignore_ascii_case(value))
    }

    /// Exact world-axis view. Horizontal views use Y up, top uses −Z up.
    pub(super) fn basis(self) -> Option<(Vec3, Vec3, Vec3)> {
        let (forward, up) = match self {
            Self::Perspective => return None,
            Self::Front => (v3(0.0, 0.0, -1.0), super::WORLD_UP),
            Self::Back => (v3(0.0, 0.0, 1.0), super::WORLD_UP),
            Self::Left => (v3(1.0, 0.0, 0.0), super::WORLD_UP),
            Self::Right => (v3(-1.0, 0.0, 0.0), super::WORLD_UP),
            Self::Top => (v3(0.0, -1.0, 0.0), v3(0.0, 0.0, -1.0)),
            Self::Bottom => (v3(0.0, 1.0, 0.0), v3(0.0, 0.0, 1.0)),
        };
        Some((forward, forward.cross(up), up))
    }
}
