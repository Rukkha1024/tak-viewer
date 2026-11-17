//! Small, name-based display layouts. No samples or file paths are persisted.
use crate::ui::graph_float::LaneSet;
use serde::{Deserialize, Serialize};
use tak_core::view_data::ViewData;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cell {
    Plate { device: String, lanes: LaneSet },
    Channel { device: String, channel: String },
}

impl Cell {
    pub fn device(&self) -> &str {
        match self {
            Self::Plate { device, .. } | Self::Channel { device, .. } => device,
        }
    }
    pub fn resolve(&self, data: &ViewData) -> Option<(usize, Vec<usize>)> {
        // Ambiguous names must not silently bind to an unrelated device/channel.
        let mut matches = data
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| d.title == self.device());
        let (di, d) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let channels = match self {
            Self::Plate { lanes, .. } => {
                d.plate?;
                let channels: Vec<_> = lanes.iter().collect();
                if channels.is_empty() || channels.iter().any(|&ci| ci >= d.channels.len()) {
                    return None;
                }
                channels
            }
            Self::Channel { channel, .. } => {
                let mut matches = d
                    .channels
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.name == *channel);
                let (ci, _) = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                vec![ci]
            }
        };
        Some((di, channels))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub name: String,
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Option<Cell>>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            name: String::new(),
            rows: 2,
            cols: 2,
            cells: vec![None; 4],
        }
    }
}
impl Layout {
    pub fn normalize(&mut self) {
        self.rows = self.rows.clamp(1, 4);
        self.cols = self.cols.clamp(1, 4);
        self.cells.resize(self.rows * self.cols, None);
    }
    pub fn resize(&mut self, n: usize) {
        self.rows = n;
        self.cols = n;
        self.normalize();
    }
    pub fn default_for(data: &ViewData) -> Self {
        let mut out = Self::default();
        let items: Vec<_> = data
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| d.plate.is_some())
            .flat_map(|(di, _)| items(data, di))
            .chain(
                data.devices
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.plate.is_none())
                    .flat_map(|(di, _)| items(data, di)),
            )
            .take(16)
            .collect();
        out.resize(if items.len() <= 4 {
            2
        } else if items.len() <= 9 {
            3
        } else {
            4
        });
        for (cell, item) in out.cells.iter_mut().zip(items) {
            *cell = Some(item);
        }
        out
    }
    pub fn initial(data: &ViewData, saved: &[Layout]) -> Self {
        saved
            .iter()
            .rev()
            .find(|l| l.cells.iter().flatten().all(|c| c.resolve(data).is_some()))
            .cloned()
            .map(|mut l| {
                l.normalize();
                l
            })
            .unwrap_or_else(|| Self::default_for(data))
    }
    /// First item replaces the target, later items fill following empty cells only.
    pub fn place(&mut self, at: usize, items: &[Cell]) {
        if at >= self.cells.len() || items.is_empty() {
            return;
        }
        self.cells[at] = Some(items[0].clone());
        let mut rest = items.iter().skip(1);
        for cell in &mut self.cells[at + 1..] {
            if cell.is_none() {
                let Some(item) = rest.next() else { break };
                *cell = Some(item.clone());
            }
        }
    }
}

pub fn items(data: &ViewData, di: usize) -> Vec<Cell> {
    let Some(d) = data.devices.get(di) else {
        return vec![];
    };
    if d.plate.is_some() && !d.channels.is_empty() {
        vec![Cell::Plate {
            device: d.title.clone(),
            lanes: LaneSet::default().shown(LaneSet::first(d.channels.len())),
        }]
    } else {
        d.channels
            .iter()
            .map(|c| Cell::Channel {
                device: d.title.clone(),
                channel: c.name.clone(),
            })
            .collect()
    }
}
pub fn channel(data: &ViewData, di: usize, ci: usize) -> Option<Cell> {
    let d = data.devices.get(di)?;
    if d.plate.is_some() && ci < 6 {
        Some(Cell::Plate {
            device: d.title.clone(),
            lanes: LaneSet::only(ci),
        })
    } else {
        Some(Cell::Channel {
            device: d.title.clone(),
            channel: d.channels.get(ci)?.name.clone(),
        })
    }
}

#[derive(Default)]
pub struct Probe {
    pub size: Option<usize>,
    pub cells: Option<String>,
    pub folded: bool,
    pub pane: Option<[f32; 2]>,
}
pub static PROBE: std::sync::OnceLock<Probe> = std::sync::OnceLock::new();
pub fn take_probe(args: &mut Vec<String>) -> Result<(), String> {
    let mut probe = Probe::default();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        if !matches!(
            flag,
            "--device-grid" | "--device-cells" | "--device-list" | "--device-pane-size"
        ) {
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("Missing device grid argument")?;
        match flag {
            "--device-grid" => {
                probe.size = Some(match value.as_str() {
                    "1x1" => 1,
                    "2x2" => 2,
                    "3x3" => 3,
                    "4x4" => 4,
                    _ => return Err("Grid must be 1x1, 2x2, 3x3 or 4x4".into()),
                })
            }
            "--device-cells" => {
                for part in value.split(',') {
                    if matches!(part, "empty" | "missing") {
                        continue;
                    }
                    let p: Vec<_> = part.split(':').collect();
                    if !(p.first() == Some(&"plate") && (p.len() == 2 || p.len() == 3)
                        || p.first() == Some(&"ch") && p.len() == 3)
                        || p[1..].iter().any(|n| n.parse::<usize>().is_err())
                    {
                        return Err(
                            "Cells: plate:N[:lane], ch:device:channel, empty, missing".into()
                        );
                    }
                }
                probe.cells = Some(value.clone());
            }
            "--device-list" if value == "folded" => probe.folded = true,
            "--device-pane-size" => {
                let (w, h) = value.split_once('x').ok_or("Pane size: WIDTHxHEIGHT")?;
                let size = [
                    w.parse::<f32>().map_err(|_| "Invalid width")?,
                    h.parse::<f32>().map_err(|_| "Invalid height")?,
                ];
                if size
                    .iter()
                    .any(|v| !v.is_finite() || *v < 100.0 || *v > 4096.0)
                {
                    return Err("Pane size must be 100..4096".into());
                }
                probe.pane = Some(size);
            }
            _ => return Err("Use --device-list folded".into()),
        }
        args.drain(i..i + 2);
    }
    if (probe.size.is_some() || probe.cells.is_some() || probe.folded || probe.pane.is_some())
        && !args
            .iter()
            .any(|a| matches!(a.as_str(), "--bench" | "--screenshot"))
    {
        return Err("Device grid probe requires --bench or --screenshot".into());
    }
    let _ = PROBE.set(probe);
    Ok(())
}
pub fn apply_probe(view: &mut crate::views::View) -> bool {
    let Some(probe) = PROBE.get().filter(|p| p.size.is_some()) else {
        return false;
    };
    let Some(data) = &view.data else { return false };
    let mut layout = Layout::default_for(data);
    layout.resize(probe.size.unwrap());
    if let Some(cells) = &probe.cells {
        layout.cells.fill(None);
        for (at, part) in cells.split(',').take(layout.cells.len()).enumerate() {
            if part == "empty" {
                continue;
            }
            let missing = || Cell::Channel {
                device: "Missing device".into(),
                channel: "Missing channel".into(),
            };
            let p: Vec<_> = part.split(':').collect();
            layout.cells[at] = if part == "missing" {
                Some(missing())
            } else {
                let di = p[1].parse::<usize>().unwrap();
                if p[0] == "plate" {
                    data.devices
                        .iter()
                        .position(|d| d.plate == Some(di))
                        .and_then(|di| {
                            if p.len() == 3 {
                                p[2].parse()
                                    .ok()
                                    .filter(|ci| *ci < 6)
                                    .and_then(|ci| channel(data, di, ci))
                            } else {
                                items(data, di).into_iter().next()
                            }
                        })
                        .or_else(|| Some(missing()))
                } else {
                    channel(data, di, p[2].parse().unwrap()).or_else(|| Some(missing()))
                }
            };
        }
    }
    view.devices.layout = Some(layout);
    view.devices.folded = Some(probe.folded || probe.pane.is_some());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cell(i: usize) -> Cell {
        Cell::Channel {
            device: "D".into(),
            channel: i.to_string(),
        }
    }
    #[test]
    fn resize_retains_reading_order_and_drops_tail() {
        let mut l = Layout::default();
        l.resize(4);
        l.cells = (0..16).map(|i| Some(cell(i))).collect();
        l.resize(2);
        assert_eq!(l.cells, (0..4).map(|i| Some(cell(i))).collect::<Vec<_>>());
        l.resize(3);
        assert_eq!(l.cells[3], Some(cell(3)));
        assert!(l.cells[4..].iter().all(Option::is_none));
        l.resize(99);
        assert_eq!((l.rows, l.cols, l.cells.len()), (4, 4, 16));
    }
    #[test]
    fn drop_replaces_target_then_only_following_empty_cells() {
        let mut l = Layout::default();
        l.cells[1] = Some(cell(9));
        l.place(0, &[cell(0), cell(1), cell(2), cell(3)]);
        assert_eq!(
            l.cells,
            vec![Some(cell(0)), Some(cell(9)), Some(cell(1)), Some(cell(2))]
        );
        l.place(99, &[cell(4)]);
        l.place(0, &[]);
        assert_eq!(l.cells[0], Some(cell(0)));
    }
    #[test]
    fn saved_roundtrip_preserves_missing_cells_and_clamps_untrusted_size() {
        let mut l = Layout::default();
        l.name = "Gait".into();
        l.cells[1] = Some(cell(42));
        let text = ron::to_string(&l).unwrap();
        let copy: Layout = ron::from_str(&text).unwrap();
        assert_eq!(l, copy);
        let data = crate::views::sample_data("a", &["M"]);
        assert!(copy.cells[1].as_ref().unwrap().resolve(&data).is_none());
        let mut empty = Layout::default();
        empty.name = "Empty workspace".into();
        empty.resize(4);
        assert_eq!(
            Layout::initial(&data, &[empty.clone()]),
            empty,
            "an explicitly saved empty grid is applicable too"
        );
        let mut bad = copy;
        bad.rows = usize::MAX;
        bad.cols = 0;
        bad.normalize();
        assert_eq!(bad.cells.len(), 4);
    }
    #[test]
    fn defaults_and_drop_items_use_structural_plate_then_analog_channels() {
        let mut data = crate::views::sample_data("a", &["M"]);
        let mut take = data.take;
        for _ in 0..2 {
            take.analog.push(tak_core::tak::AnalogDevice {
                channels: (0..6)
                    .map(|i| tak_core::tak::AnalogChannel {
                        name: format!("C{i}"),
                        kind: "Analog".into(),
                        dim: 1,
                        frames: vec![].into(),
                        values: vec![].into(),
                    })
                    .collect(),
            });
        }
        data = ViewData::build(take);
        data.devices[1].plate = Some(0);
        let l = Layout::default_for(&data);
        assert_eq!(l.rows, 3);
        assert!(matches!(&l.cells[0],Some(Cell::Plate{lanes,..}) if *lanes==LaneSet::default()));
        assert_eq!(l.cells[1], Some(channel(&data, 0, 0).unwrap()));
        assert_eq!(items(&data, 0).len(), 6);
        assert_eq!(items(&data, 1).len(), 1);
        let single = channel(&data, 1, 4).unwrap();
        assert_eq!(single.resolve(&data), Some((1, vec![4])));
        let mut named = l.clone();
        named.name = "Saved".into();
        assert_eq!(Layout::initial(&data, &[named.clone()]), named);
        assert_eq!(
            Layout::default_for(&crate::views::sample_data("empty", &["M"])).rows,
            2
        );
    }
}
