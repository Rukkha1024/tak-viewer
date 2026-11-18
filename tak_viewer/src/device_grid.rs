//! Small display layouts. Indexed channels; legacy name cells remain readable. No samples or file paths are persisted.
use crate::ui::graph_float::LaneSet;
use serde::{Deserialize, Serialize};
use tak_core::view_data::ViewData;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cell {
    Plate { device: String, lanes: LaneSet },
    Channel { device: String, channel: String },
    ChannelIndex { device: String, index: usize },
}

impl Cell {
    pub fn device(&self) -> &str {
        match self {
            Self::Plate { device, .. }
            | Self::Channel { device, .. }
            | Self::ChannelIndex { device, .. } => device,
        }
    }
    pub fn resolve(&self, data: &ViewData) -> Option<(usize, Vec<usize>)> {
        // Ambiguous names must not silently bind to an unrelated device/channel.
        let mut matches = data.devices.iter().enumerate().filter(|(di, d)| {
            d.title == self.device()
                || (d.plate.is_none() && self.device() == format!("아날로그 장치 {}", di + 1))
        });
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
            Self::ChannelIndex { index, .. } => {
                d.channels.get(*index)?;
                vec![*index]
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
                l.canonicalize(data);
                l
            })
            .unwrap_or_else(|| Self::default_for(data))
    }
    /// Upgrade resolvable legacy identities when loading/saving; retain absent identities.
    pub fn canonicalize(&mut self, data: &ViewData) {
        for slot in &mut self.cells {
            if let Some(cell @ Cell::Channel { .. }) = slot.as_ref() {
                if let Some((di, channels)) = cell.resolve(data) {
                    *slot = Some(Cell::ChannelIndex {
                        device: data.devices[di].title.clone(),
                        index: channels[0],
                    });
                }
            }
        }
    }
    /// Same destination plan drives paint and release; occupied later cells stay untouched.
    pub fn destinations(&self, at: usize, count: usize) -> Vec<usize> {
        if at >= self.cells.len() || count == 0 {
            return vec![];
        }
        std::iter::once(at)
            .chain((at + 1..self.cells.len()).filter(|&i| self.cells[i].is_none()))
            .take(count)
            .collect()
    }
    pub fn move_or_swap(&mut self, from: usize, to: usize) {
        if from < self.cells.len() && to < self.cells.len() && self.cells[from].is_some() {
            self.cells.swap(from, to);
        }
    }
    pub fn fill_device(&mut self, data: &ViewData, di: usize) {
        let items = items(data, di);
        let count = items.len().min(16);
        let n = (1..=4).find(|n| n * n >= count).unwrap_or(4);
        self.resize(n);
        self.cells.fill(None);
        for (slot, item) in self.cells.iter_mut().zip(items) {
            *slot = Some(item);
        }
    }
    /// First item replaces the target, later items fill following empty cells only.
    pub fn place(&mut self, at: usize, items: &[Cell]) {
        if at >= self.cells.len() || items.is_empty() {
            return;
        }
        for (at, item) in self.destinations(at, items.len()).into_iter().zip(items) {
            self.cells[at] = Some(item.clone());
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
            .enumerate()
            .map(|(index, _)| Cell::ChannelIndex {
                device: d.title.clone(),
                index,
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
        d.channels.get(ci)?;
        Some(Cell::ChannelIndex {
            device: d.title.clone(),
            index: ci,
        })
    }
}

/// Small metadata only; app.ron never contains samples or modified file channel names.
pub type ChannelNames =
    std::collections::BTreeMap<String, std::collections::BTreeMap<usize, String>>;
pub fn display_name<'a>(
    names: &'a ChannelNames,
    data: &'a ViewData,
    di: usize,
    ci: usize,
) -> &'a str {
    names
        .get(&data.devices[di].title)
        .and_then(|n| n.get(&ci))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(&data.devices[di].channels[ci].name)
}

#[derive(Default)]
pub struct Probe {
    pub size: Option<usize>,
    pub fill: Option<usize>,
    pub names: Vec<(usize, usize, String)>,
    pub drag: Option<(usize, usize)>,
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
            "--device-grid"
                | "--device-cells"
                | "--device-list"
                | "--device-pane-size"
                | "--device-fill"
                | "--channel-names"
                | "--device-drag"
        ) {
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("Missing device grid argument")?;
        match flag {
            "--device-fill" => {
                probe.fill = Some(value.parse().map_err(|_| "Fill device index required")?);
                probe.size.get_or_insert(4);
            }
            "--channel-names" => {
                for item in value.split(',') {
                    let (key, name) = item
                        .split_once('=')
                        .ok_or("Names: device:channel=Name,...")?;
                    let (di, ci) = key
                        .split_once(':')
                        .ok_or("Names: device:channel=Name,...")?;
                    probe.names.push((
                        di.parse().map_err(|_| "Invalid device")?,
                        ci.parse().map_err(|_| "Invalid channel")?,
                        name.to_owned(),
                    ));
                }
            }
            "--device-drag" => {
                let (di, at) = value.split_once(':').ok_or("Drag: device:target-cell")?;
                probe.drag = Some((
                    di.parse().map_err(|_| "Invalid device")?,
                    at.parse().map_err(|_| "Invalid target")?,
                ));
            }
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
    if (probe.size.is_some()
        || probe.cells.is_some()
        || probe.folded
        || probe.pane.is_some()
        || !probe.names.is_empty()
        || probe.drag.is_some())
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
    if let Some(di) = probe.fill {
        layout.fill_device(data, di);
    }
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
    view.devices.reconcile();
    true
}

pub fn apply_names(data: &ViewData, names: &mut ChannelNames) {
    if let Some(probe) = PROBE.get() {
        for (di, ci, name) in &probe.names {
            if let Some(d) = data.devices.get(*di).filter(|d| *ci < d.channels.len()) {
                names
                    .entry(d.title.clone())
                    .or_default()
                    .insert(*ci, name.clone());
            }
        }
    }
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
    fn analog(count: usize) -> ViewData {
        let mut take = crate::views::sample_data("a", &["M"]).take;
        take.analog.push(tak_core::tak::AnalogDevice {
            channels: (0..count)
                .map(|i| tak_core::tak::AnalogChannel {
                    name: format!("AnalogChannel{i}"),
                    kind: "Analog".into(),
                    dim: 1,
                    frames: vec![].into(),
                    values: vec![].into(),
                })
                .collect(),
        });
        ViewData::build(take)
    }
    #[test]
    fn native_device_name_keeps_layout_and_names_when_device_order_changes() {
        let mut first = analog(16).take;
        first.device_names = vec![Some("NIDAQ USB-6351".into())];
        let a = ViewData::build(first.clone());
        let cell = channel(&a, 0, 0).unwrap();
        let mut names = ChannelNames::default();
        names
            .entry("NIDAQ USB-6351".into())
            .or_default()
            .insert(0, "L TA".into());
        first.analog.insert(0, first.analog[0].clone());
        first.device_names.insert(0, Some("Other device".into()));
        let b = ViewData::build(first);
        assert_eq!(cell.resolve(&b), Some((1, vec![0])));
        assert_eq!(display_name(&names, &b, 1, 0), "L TA");
        let mut legacy = Layout::default();
        legacy.cells[0] = Some(Cell::Channel {
            device: "아날로그 장치 2".into(),
            channel: "AnalogChannel0".into(),
        });
        legacy.canonicalize(&b);
        assert_eq!(legacy.cells[0], Some(cell));
    }
    #[test]
    fn fill_smallest_square_caps_channels_and_preserves_order() {
        for (count, size) in [(16, 4), (8, 3), (20, 4)] {
            let data = analog(count);
            let mut layout = Layout::default();
            layout.fill_device(&data, 0);
            assert_eq!((layout.rows, layout.cols), (size, size));
            assert_eq!(layout.cells.iter().flatten().count(), count.min(16));
            for (ci, cell) in layout.cells.iter().flatten().enumerate() {
                assert_eq!(cell.resolve(&data), Some((0, vec![ci])));
            }
        }
    }
    #[test]
    fn swap_move_replace_preview_agree_and_invalid_source_is_noop() {
        let mut layout = Layout::default();
        layout.cells = vec![Some(cell(0)), Some(cell(1)), None, None];
        layout.move_or_swap(0, 1);
        assert_eq!(layout.cells[..2], [Some(cell(1)), Some(cell(0))]);
        layout.move_or_swap(1, 2);
        assert_eq!(layout.cells[1], None);
        assert_eq!(layout.cells[2], Some(cell(0)));
        let before = layout.clone();
        layout.move_or_swap(1, 3);
        layout.move_or_swap(99, 3);
        assert_eq!(layout, before);
        assert_eq!(layout.destinations(0, 3), vec![0, 1, 3]);
        layout.place(0, &[cell(8), cell(9), cell(10)]);
        assert_eq!(
            layout.cells,
            vec![Some(cell(8)), Some(cell(9)), Some(cell(0)), Some(cell(10))]
        );
    }
    #[test]
    fn names_fallback_and_index_layout_survives_file_and_user_rename() {
        let mut data = analog(2);
        let mut legacy = Layout::default();
        legacy.cells[0] = Some(Cell::Channel {
            device: data.devices[0].title.clone(),
            channel: "AnalogChannel1".into(),
        });
        legacy.canonicalize(&data);
        assert!(matches!(
            legacy.cells[0],
            Some(Cell::ChannelIndex { index: 1, .. })
        ));
        let mut names = ChannelNames::default();
        let cell = channel(&data, 0, 1).unwrap();
        assert_eq!(display_name(&names, &data, 0, 1), "AnalogChannel1");
        names
            .entry(data.devices[0].title.clone())
            .or_default()
            .insert(1, " L TA ".into());
        data.devices[0].channels[1].name = "Changed file name".into();
        assert_eq!(display_name(&names, &data, 0, 1), "L TA");
        let copy: Cell = ron::from_str(&ron::to_string(&cell).unwrap()).unwrap();
        assert_eq!(copy.resolve(&data), Some((0, vec![1])));
        names
            .get_mut(&data.devices[0].title)
            .unwrap()
            .insert(1, "  ".into());
        assert_eq!(display_name(&names, &data, 0, 1), "Changed file name");
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
