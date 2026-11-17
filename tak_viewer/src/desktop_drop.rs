//! OS drag input. winit's Windows OLE DragOver discards its POINTL, so sample only
//! during a file hover/drop. No filesystem work or native cursor call on idle frames.
use eframe::egui::{self, Pos2, RawInput, Rect};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, OnceLock};
use crate::ui::dock::{self, DropZone};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    View(usize, DropZone),
    NewTab,
    /// Empty editor, outside a view, or a backend with no pointer position.
    Fallback,
}

pub fn supported(path: &Path) -> bool { crate::workspace::is_take_file(&path.to_string_lossy()) }

pub fn target(position: Option<Pos2>, strip: Option<Rect>, views: &[(usize, Rect)]) -> Target {
    let Some(p) = position else { return Target::Fallback };
    if strip.is_some_and(|r| r.contains(p)) { return Target::NewTab; }
    views.iter().find(|(_, r)| r.contains(p)).map_or(Target::Fallback,
        |(slot, r)| Target::View(*slot, dock::drop_zone(*r, p)))
}

#[derive(Default)]
pub struct Hover {
    pub paths: Vec<PathBuf>,
    pub position: Option<Pos2>,
    pub identities: Option<Vec<PathBuf>>,
    identity_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
    #[cfg(windows)]
    hwnd: Option<isize>,
    #[cfg(windows)]
    native_position: Option<Pos2>,
}

impl Hover {
    #[cfg(windows)]
    pub fn set_window(&mut self, cc: &eframe::CreationContext<'_>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        self.hwnd = cc.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
            _ => None,
        });
    }

    pub fn native_input(&mut self, ctx: &egui::Context, raw: &mut RawInput) {
        #[cfg(windows)] {
            self.native_position = None;
            let relevant = raw.hovered_files.iter().any(|f| f.path.as_deref().is_some_and(supported))
                || raw.dropped_files.iter().any(|f| f.path.as_deref().is_some_and(supported));
            if relevant {
                if let Some(hwnd) = self.hwnd {
                    use windows_sys::Win32::{Foundation::POINT, Graphics::Gdi::ScreenToClient, UI::WindowsAndMessaging::GetCursorPos};
                    let mut p = POINT { x: 0, y: 0 };
                    // SAFETY: eframe owns this HWND for the app's lifetime; POINT is writable.
                    if unsafe { GetCursorPos(&mut p) != 0 && ScreenToClient(hwnd, &mut p) != 0 } {
                        let at = client_points(p.x, p.y, ctx.pixels_per_point());
                        self.native_position = Some(at);
                        raw.events.push(egui::Event::PointerMoved(at));
                    }
                }
            }
        }
        #[cfg(not(windows))] let _ = (ctx, raw);
    }

    /// Called for both native and RawInput-driven headless frames. Cancel clears all state.
    pub fn read(&mut self, ctx: &egui::Context) {
        let (paths, dropped) = ctx.input(|i| (
            i.raw.hovered_files.iter().filter_map(|f| f.path.as_ref())
                .filter(|p| supported(p)).cloned().collect::<Vec<_>>(),
            !i.raw.dropped_files.is_empty(),
        ));
        if paths.is_empty() && !dropped {
            self.paths.clear();
            self.identities = None;
            self.identity_rx = None;
            self.position = None;
            return;
        }
        let position = ctx.input(|i| i.pointer.latest_pos());
        #[cfg(windows)] let position = self.native_position.or(position);
        self.position = position.or(self.position);
        if paths != self.paths {
            self.paths = paths.clone();
            self.identities = None;
            self.identity_rx = None;
            if !paths.is_empty() {
                let (tx, rx) = mpsc::channel();
                self.identity_rx = Some(rx);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let identities = paths.into_iter().map(|p| tak_core::takm::canonical_target(&p).unwrap_or(p)).collect();
                    let _ = tx.send(identities);
                    ctx.request_repaint();
                });
            }
        }
        if let Some(rx) = &self.identity_rx {
            if let Ok(ids) = rx.try_recv() { self.identities = Some(ids); self.identity_rx = None; }
        }
        if !self.paths.is_empty() { ctx.request_repaint_after(std::time::Duration::from_millis(16)); }
    }
}

#[cfg(any(windows, test))]
fn client_points(x: i32, y: i32, pixels_per_point: f32) -> Pos2 {
    egui::pos2(x as f32 / pixels_per_point, y as f32 / pixels_per_point)
}

#[derive(Debug)]
struct Preview { position: Pos2, files: Vec<PathBuf> }
static PREVIEW: OnceLock<Preview> = OnceLock::new();

/// Agent-only: consume the final `--drop-preview x,y incoming...` group.
pub fn take_preview(args: &mut Vec<String>) -> Result<(), &'static str> {
    let Some(i) = args.iter().position(|a| a == "--drop-preview") else { return Ok(()) };
    let error = "--drop-preview: use --headless --screenshot out.png <open files> --drop-preview x,y <incoming files>";
    if !args.iter().any(|a| a == "--screenshot") || !args.iter().any(|a| a == "--headless") { return Err(error); }
    let (x, y) = args.get(i + 1).and_then(|s| s.split_once(',')).ok_or(error)?;
    let position = egui::pos2(x.parse().map_err(|_| error)?, y.parse().map_err(|_| error)?);
    if !position.x.is_finite() || !position.y.is_finite() { return Err(error); }
    let files: Vec<_> = args[i + 2..].iter().map(|a| crate::workspace::resolve_path(a)).collect();
    if files.is_empty() || files.iter().any(|p| !supported(p)) { return Err(error); }
    PREVIEW.set(Preview { position, files }).map_err(|_| error)?;
    args.truncate(i);
    Ok(())
}

pub fn inject_preview(raw: &mut RawInput) {
    if let Some(preview) = PREVIEW.get() {
        raw.hovered_files = preview.files.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), ..Default::default() }).collect();
        raw.events.push(egui::Event::PointerMoved(preview.position));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positions_choose_all_edges_center_and_strip_first() {
        let r = Rect::from_min_max(egui::pos2(100.0, 40.0), egui::pos2(900.0, 640.0));
        for (x, y, zone) in [(0.05,0.5,DropZone::Left),(0.95,0.5,DropZone::Right),(0.5,0.05,DropZone::Above),(0.5,0.95,DropZone::Below),(0.5,0.5,DropZone::Center)] {
            assert_eq!(target(Some(r.lerp_inside(egui::vec2(x,y))), None, &[(2,r)]), Target::View(2,zone));
        }
        assert_eq!(target(Some(r.center()),Some(r),&[(2,r)]),Target::NewTab);
        assert_eq!(target(None,Some(r),&[(2,r)]),Target::Fallback);
        assert_eq!(target(Some(Pos2::ZERO),None,&[(2,r)]),Target::Fallback);
        assert_eq!(client_points(450,300,1.5),egui::pos2(300.0,200.0));
    }
    #[test]
    fn idle_does_not_schedule_work_and_cancel_forgets_last_position() {
        let ctx = egui::Context::default();
        let mut hover = Hover::default();
        let _ = ctx.run(RawInput::default(), |ctx| hover.read(ctx));
        assert!(hover.identity_rx.is_none() && hover.paths.is_empty() && hover.position.is_none());
        hover.position = Some(Pos2::ZERO);
        let _ = ctx.run(RawInput::default(), |ctx| hover.read(ctx));
        assert!(hover.position.is_none());
    }
}
