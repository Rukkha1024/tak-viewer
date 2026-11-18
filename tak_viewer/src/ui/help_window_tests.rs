use super::*;
use std::collections::HashSet;

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '/' | ',' | '(' | ')' | '`'))
        .filter(|token| !token.is_empty())
}

fn is_key(token: &str) -> bool {
    if token.starts_with("Ctrl+") || token.starts_with("Shift+") || token.starts_with("Alt+") {
        return true;
    }
    matches!(
        token,
        "Space"
            | "Delete"
            | "Esc"
            | "Enter"
            | "Home"
            | "End"
            | "PageDown"
            | "PageUp"
            | "→"
            | "←"
            | "↑"
            | "↓"
            | "["
            | "]"
            | "1"
            | "2"
    ) || (token.len() == 1 && token.as_bytes()[0].is_ascii_uppercase())
        || token
            .strip_prefix('F')
            .is_some_and(|number| number.parse::<u8>().is_ok())
}

#[test]
fn help_keys_match_readme_and_rows_are_unique() {
    let readme = include_str!("../../README.md");
    let controls = readme
        .split_once("\n## Controls\n")
        .unwrap()
        .1
        .split("\n## ")
        .next()
        .unwrap();
    let documented: HashSet<_> = controls
        .lines()
        .filter_map(|line| {
            line.strip_prefix("| ")?
                .split_once(" | ")
                .map(|(inputs, _)| inputs)
        })
        .flat_map(tokens)
        .collect();
    let mut input_rows = HashSet::new();
    for (_, rows) in SECTIONS {
        for &(input, _) in *rows {
            assert!(input_rows.insert(input), "duplicate help row: {input}");
            for key in tokens(input).filter(|key| is_key(key)) {
                assert!(
                    documented.contains(key),
                    "undocumented input {key} in {input}"
                );
            }
        }
    }
}

#[test]
fn help_section_copy_uses_english_and_input_symbols() {
    for (heading, rows) in SECTIONS {
        let texts = std::iter::once(*heading).chain(rows.iter().flat_map(|&(input, action)| [input, action]));
        for text in texts {
            assert!(
                text.chars().all(|c| c.is_ascii() || "→←↑↓×".contains(c)),
                "unexpected character in help text: {text}"
            );
        }
    }
}

fn input(size: [f32; 2]) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size.into())),
        ..Default::default()
    }
}

fn draw(ctx: &egui::Context, raw: egui::RawInput) -> (Rect, egui::FullOutput) {
    let screen = raw.screen_rect.unwrap();
    let workspace = Rect::from_min_max(screen.min + vec2(0.0, 26.0), screen.max - vec2(0.0, 22.0));
    let mut rect = Rect::NOTHING;
    let out = ctx.run(raw, |ctx| {
        rect = show(ctx, &mut true, workspace).unwrap().rect;
    });
    assert!(
        workspace.contains_rect(rect),
        "{rect:?} outside {workspace:?}"
    );
    (rect, out)
}

#[test]
fn help_fits_small_viewports_and_stays_inside_after_shrink() {
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    for size in [[1280.0, 720.0], [800.0, 600.0], [320.0, 240.0]] {
        for _ in 0..3 {
            draw(&ctx, input(size));
        }
    }
}

fn visible_text(shapes: &[egui::epaint::ClippedShape], text: &str) -> bool {
    shapes.iter().any(|shape| {
        if let egui::Shape::Text(label) = &shape.shape {
            label.galley.job.text == text
                && shape
                    .clip_rect
                    .contains_rect(label.galley.rect.translate(label.pos.to_vec2()))
        } else {
            false
        }
    })
}

#[test]
fn wheel_reaches_last_help_row_and_keeps_title_visible() {
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    theme::install_fonts(&ctx);
    let size = [800.0, 600.0];
    let last_row = SECTIONS.last().unwrap().1.last().unwrap().0;
    draw(&ctx, input(size));
    draw(&ctx, input(size));
    let (rect, before) = draw(&ctx, input(size));
    assert!(!visible_text(&before.shapes, last_row));

    let mut scroll = input(size);
    scroll.events = vec![
        egui::Event::PointerMoved(rect.center()),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -6000.0),
            modifiers: egui::Modifiers::NONE,
        },
    ];
    draw(&ctx, scroll);
    let (after_rect, after) = draw(&ctx, input(size));
    assert_eq!(rect, after_rect);
    assert!(visible_text(&after.shapes, "Controls"));
    assert!(visible_text(&after.shapes, last_row));
}

#[test]
fn help_symbols_have_glyphs_in_the_theme() {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    let _ = ctx.run(input([800.0, 600.0]), |ctx| {
        assert!(ctx.fonts(|fonts| fonts.has_glyphs(&egui::FontId::proportional(14.0), "→←↑↓▼·×")));
    });
}
