use eframe::egui::{self, vec2, Align, Label, Layout, Rect, RichText};

use super::theme;

const DEFAULT_BODY: [f32; 2] = [610.0, 540.0];
const KEY_WIDTH: f32 = 154.0;
const COLUMN_GAP: f32 = 12.0;
const OVERLAY_INPUT: &str = "Explorer 파일 오른쪽 클릭 → 이 화면에 얹기";

pub(crate) fn show(ctx: &egui::Context, open: &mut bool, workspace: Rect) -> Option<egui::Response> {
    let bounds = workspace.shrink(8.0);
    let style = ctx.style();
    let frame = egui::Frame::window(&style);
    let title_height = egui::TextStyle::Heading.resolve(&style).size + frame.inner_margin.sum().y;
    // Leave room for the frame and pixel rounding outside the scrollable body.
    let chrome = frame.total_margin().sum() + vec2(2.0, title_height + 2.0);
    let max_body = (bounds.size() - chrome).max(egui::Vec2::ZERO);
    let body = egui::Vec2::from(DEFAULT_BODY).min(max_body);

    egui::Window::new("조작키")
        .open(open)
        .collapsible(false)
        .default_pos(bounds.center() - 0.5 * (body + chrome))
        .default_size(body)
        .min_size(vec2(320.0, 144.0).min(max_body))
        .max_size(max_body)
        .constrain_to(bounds)
        .show(ctx, |ui| {
            ui.spacing_mut().scroll = egui::style::ScrollStyle {
                foreground_color: true,
                ..egui::style::ScrollStyle::solid()
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, draw_sections);
        })
        .map(|window| window.response)
}

fn draw_sections(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let key_width = KEY_WIDTH.min(width * 0.34);
    let description_width = (width - key_width - COLUMN_GAP).max(0.0);
    ui.spacing_mut().item_spacing.x = COLUMN_GAP;
    for (index, (title, rows)) in SECTIONS.iter().enumerate() {
        if index > 0 {
            ui.add_space(10.0);
            ui.separator();
        }
        ui.label(RichText::new(*title).strong().color(theme::ACCENT));
        ui.add_space(3.0);
        for &(input, action) in *rows {
            let description = if input == OVERLAY_INPUT {
                RichText::new(format!("{action} 서로 다른 파일은 전체에서 {}개까지", crate::MAX_OPEN_FILES))
            } else {
                RichText::new(action)
            };
            ui.horizontal_top(|ui| {
                for (width, text) in [
                    (key_width, RichText::new(input).strong()),
                    (description_width, description),
                ] {
                    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(width);
                        ui.add(Label::new(text).wrap());
                    });
                }
            });
        }
    }
}

type Rows = &'static [(&'static str, &'static str)];

const SECTIONS: &[(&str, Rows)] = &[
    ("3D 화면", &[
        ("왼쪽 클릭 / Ctrl+클릭 / Shift+클릭", "마커 선택 / 선택 켜고 끄기 / 선택에 더하기"),
        ("Alt+클릭", "라벨을 붙이지 않고 마커 하나 보기"),
        ("Ctrl+A", "마지막으로 누른 3D 화면에서 지금 보이는 마커 모두 선택"),
        ("왼쪽 드래그 / Ctrl+드래그", "네모 안 마커 선택 / 기존 선택에 더하기"),
        ("오른쪽 드래그", "가운데 점을 중심으로 회전"),
        ("가운데 버튼 드래그 / 휠", "화면 이동 / 확대·축소"),
        ("더블클릭 또는 F / Shift+F", "선택 마커에 맞추기 / 전체 보기"),
        ("G / 보기 → Follow Selected (G)", "선택한 마커 따라가기 켜고 끄기"),
        ("L / 보기 → Marker Names (L)", "마커 이름 켜고 끄기. 꺼도 가리킨 마커의 이름은 보여요"),
        ("보기 → View Direction", "Perspective / Front / Back / Left / Right / Top / Bottom 중 고르기"),
        ("보기 → Distance / Angle", "두 점의 거리 또는 세 점의 각도 켜고 끄기. 각도 꼭짓점은 두 번째로 골라요"),
        ("3D 마커 오른쪽 클릭", "Marker Info에서 좌표 보기. Label Marker / Unlabel Marker(s) / Remove Marker로 편집"),
        ("힘판 클릭 / Devices의 3D", "힘판 그래프 열기 / 다시 누르면 닫기"),
    ]),
    ("재생과 프레임", &[
        ("Space / Reverse", "재생·정지 / 재생 방향 바꾸기"),
        ("→ / ← / ↑ / ↓", "다음·이전 1 프레임 / 다음·이전 10 프레임"),
        ("Home / End", "첫 프레임 / 마지막 프레임"),
        ("재생 막대 위 칸 클릭·드래그", "그 프레임으로 이동"),
        ("재생 막대 아래 띠 드래그·클릭", "노란 편집 구간 잡기 / 빈 곳을 눌러 구간 풀기"),
        ("[ 또는 V / ] 또는 B", "지금 프레임을 편집 구간의 시작 / 끝으로"),
        ("Shift+B / Shift+V", "편집 구간 풀기 / 전체 파일을 편집 구간으로"),
        ("초록 손잡이 / Working Range", "손잡이를 끌거나 Start·End 숫자를 바꿔 재생 구간 정하기"),
        ("Shift+[ / Shift+] / Reset", "재생 구간을 전체 파일로 되돌리기"),
        ("Z / Shift+Z", "선택 마커의 다음 빈 구간 시작 / 이전 빈 구간 끝으로"),
    ]),
    ("탭과 파일", &[
        ("Ctrl+O / Ctrl+Shift+O / Ctrl+L", "파일 열기 / 폴더 추가 / 경로로 열기"),
        ("Ctrl+T / Ctrl+Shift+T", "빈 탭 만들기 / 마지막으로 닫은 화면 또는 탭 다시 열기"),
        ("Ctrl+Tab / Ctrl+PageDown", "다음 탭"),
        ("Ctrl+Shift+Tab / Ctrl+PageUp", "이전 탭"),
        ("위쪽 탭 줄에서 휠", "아래로 굴리면 뒤쪽 탭, 위로 굴리면 앞쪽 탭이 보여요. Shift와 휠 또는 가로 스크롤도 돼요"),
        ("F2", "현재 탭 이름 바꾸기. Enter로 확정, Esc로 취소"),
        ("Ctrl+W", "선택한 3D 화면 닫기. 마지막 화면이면 탭 닫기"),
        ("PageDown / PageUp", "같은 폴더의 다음 / 이전 파일"),
        ("Explorer 클릭 / Ctrl+클릭", "지금 화면에 열기 / 옆 화면에 열기 (3D 화면 최대 4개, 2×2까지)"),
        (OVERLAY_INPUT, "그 파일의 마커를 고른 화면 위에 흰 고리로 겹쳐 보기 (같은 프레임, 프레임 수 · 속도가 같은 파일만). 얹은 파일 내리기 메뉴 또는 이름표 2의 ×로 제거."),
        ("1 / 2 또는 이름표", "처음 파일 / 얹은 파일의 마커 숨기기·다시 보이기"),
        ("3D 화면 클릭 / 화면 탭 닫기", "키와 패널이 따를 화면 고르기 / 그 화면 닫기"),
        ("저장 폴더 연필 / 오른쪽 클릭", "Explorer에서 폴더에 보이는 이름 바꾸기"),
    ]),
    ("그래프", &[
        ("Channels / Tracks / Combined", "좌표별 선 / 마커별 기록 띠 / 세 좌표를 한 칸에 보기"),
        ("그래프 클릭 / 왼쪽 드래그", "프레임 이동 / 노란 편집 구간 선택"),
        ("그래프 휠 / 가운데 버튼 드래그", "시간 축 확대·축소 / 이동. Tracks는 시간 눈금 위에서 조작"),
        ("Tracks 행 위에서 휠", "마커 목록 스크롤"),
        ("Tracks 행의 이름 오른쪽에서 왼쪽 드래그", "가로로 끌어 노란 편집 구간 선택. 고른 마커와 현재 프레임은 그대로"),
        ("그래프 오른쪽 드래그 / Auto Extents", "누른 칸의 값 축 확대·축소 / 값 축을 처음 크기로"),
        ("그래프에서 F / Shift+F", "그래프를 누른 뒤 편집 구간 / 전체 파일에 맞추기"),
        ("Zoom Fit / Lock Selection", "편집 구간에 맞추기 / Channels·Combined에 표시할 마커 고정·풀기"),
        ("Merge Keys Up / Merge Keys Down", "Tracks의 두 행에서 아래 / 위 행의 점을 다른 행으로 옮기기. 같은 프레임에 점이 겹치면 중단"),
        ("3D 위 그래프 이름표·모서리 끌기", "이름표로 이동, 모서리로 크기 조절. 이름표 두 번 누르면 접기·펴기"),
        ("3D 위 그래프 안 클릭·드래그 / 휠", "프레임 이동 / 시간 축 확대·축소"),
        ("3D 위 그래프에 마우스 올리기", "접기·펴기, 마커 그래프를 Graph (Below 3D)로 보내기, 힘판 그래프의 줄 선택·닫기"),
    ]),
    ("마커 편집", &[
        ("목록에서 Ctrl+클릭 / Shift+클릭 / Ctrl+A", "선택 켜고 끄기 / 사이 행 선택 / 모두 선택"),
        ("Tracks에서 Ctrl+Shift+클릭", "기존 선택에 사이 행을 더하기"),
        ("D / Q", "QuickLabel Mode와 Select Mode 전환 / Select Mode로 바꾸기"),
        ("S / A", "다음 / 이전 라벨"),
        ("Shift+D", "선택 마커의 라벨 떼기"),
        ("Delete", "마지막으로 누른 곳에 따라 마커 점·Comment 지우기 또는 열린 파일 닫기"),
        ("C / Shift+C", "Cubic / Pattern Based로 빈 구간 채우기"),
        ("X / Shift+X / Ctrl+Alt+X", "선택 마커의 구간 / 모든 마커의 구간 / 모든 마커의 전체 움직임을 부드럽게"),
        ("Ctrl+S", "현재 편집 저장"),
    ]),
    ("Comment", &[
        ("M 짧게 / Comment 버튼", "고른 구간이나 현재 프레임에 Comment 만들기"),
        ("M 누르고 있기", "프레임을 움직이다 놓으면 그동안 지난 구간에 Comment 만들기"),
        ("유형 버튼 / T", "만든 직후 유형 고르기 / 선택한 Comment의 유형 차례로 바꾸기"),
        ("재생 막대의 주황 칸 클릭 / 오른쪽 클릭", "Comment 첫 프레임으로 이동 / Comment 지우기"),
        ("COMMENTS 행 클릭 / # 두 번 클릭", "Comment와 마커 선택 / 첫 프레임으로 이동"),
        ("COMMENTS 칸 두 번 클릭 / Enter", "프레임·마커·글 고치기 / 저장. Type은 눌러서 고르기"),
        ("Comment 저장 위치", "Comment는 .takm 파일 안에 저장해요."),
    ]),
    ("그 밖", &[
        ("Esc", "진행 중인 입력·편집 먼저 취소. 그다음 선택이나 편집 구간 풀기"),
        ("Ctrl+Z 또는 Ctrl+Alt+Z / Ctrl+Y", "되돌리기 / 다시 하기"),
        ("Ctrl+B / Ctrl+J / Ctrl+Alt+B", "왼쪽 / 아래 / 오른쪽 패널 켜고 끄기"),
        ("패널 제목 끌기", "패널 옮기기, 탭으로 합치기, 떠 있는 창으로 떼기"),
        ("Ctrl+Shift+S", "지금 각도와 위·옆·앞의 화면 캡처 저장"),
    ]),
];

#[cfg(test)]
#[path = "help_window_tests.rs"]
mod tests;
