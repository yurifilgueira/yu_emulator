//! Draws the menu: the library (a list of games and a panel about the
//! selected one) or, without games, the screen that asks for a folder.
//!
//! Everything is painted by hand at the positions of the design mockup;
//! egui is used for text, shapes, animation and mouse clicks.

use super::icons::{self, Face};
use super::menu::{Action, EmptyButton, Menu};
use super::theme::*;
use crate::library::{display_path, format_size};
use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Galley, Painter, Rect, Response, Sense, Ui};
use std::path::PathBuf;
use std::sync::Arc;

/// What the menu shows about the machine, besides the games.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Controllers connected.
    pub controllers: usize,
    /// Whether a memory card file backs slot 1.
    pub memory_card: bool,
    /// The home folder, written as `~` in paths.
    pub home: Option<PathBuf>,
}

/// Height of the line above the list (folder and order).
const LIST_HEADER: f32 = 40.0;
/// Game tile in a list row, and the space around it.
const TILE: f32 = 40.0;
const ROW_INSET: f32 = 12.0;
/// Padding inside the details panel and height of its cover and button.
const PANEL_INSET: f32 = 20.0;
const COVER_HEIGHT: f32 = 200.0;
const BUTTON_HEIGHT: f32 = 48.0;
/// The card of the screen without games.
const EMPTY_CARD: eframe::egui::Vec2 = vec2(440.0, 412.0);
/// Room kept under the list for a message.
const MESSAGE_HEIGHT: f32 = 56.0;

/// Draws the menu over the whole window. Returns what a mouse click asked for.
pub fn show(ui: &mut Ui, menu: &mut Menu, status: &Status) -> Option<Action> {
    let screen = ui.ctx().content_rect();
    let painter = ui.ctx().layer_painter(ui.layer_id());
    painter.rect_filled(screen, 0.0, BG);
    let middle = Rect::from_min_max(pos2(screen.left(), screen.top() + TOP_BAR), pos2(screen.right(), screen.bottom() - FOOTER));
    let empty = menu.games.is_empty();
    let action = if empty { empty_screen(ui, &painter, middle, menu, status) } else { library(ui, &painter, middle.shrink(PADDING), menu, status) };
    top_bar(&painter, screen, menu.games.len(), status);
    footer(&painter, screen, empty);
    action
}

/// Text on one line, cut with an ellipsis if wider than `width`.
fn elided(painter: &Painter, text: &str, font: FontId, colour: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, colour);
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    painter.fonts_mut(|fonts| fonts.layout_job(job))
}

/// Draws one line of text starting at `left`, centred on `centre_y`.
fn line(painter: &Painter, left: f32, centre_y: f32, galley: Arc<Galley>) {
    let top = centre_y - galley.size().y / 2.0;
    painter.galley(pos2(left, top), galley, TEXT);
}

/// Wrapped text with every line centred on `centre_x`, at most two lines.
fn centred(painter: &Painter, centre_x: f32, top: f32, text: &str, font: FontId, colour: Color32, width: f32) -> f32 {
    let font_size = font.size;
    let mut job = LayoutJob::simple(text.to_owned(), font, colour, width);
    job.halign = Align::Center;
    job.wrap.max_rows = 2;
    job.sections[0].format.line_height = Some((font_size * 1.45).round());
    let galley = painter.fonts_mut(|fonts| fonts.layout_job(job));
    let height = galley.size().y;
    painter.galley(pos2(centre_x, top), galley, colour);
    height
}

/// A clickable area; the pointer becomes a hand over it.
fn clickable(ui: &Ui, rect: Rect, name: impl std::hash::Hash + std::fmt::Debug) -> Response {
    // Not focusable: Tab and the arrows belong to the pad, not to egui.
    ui.interact(rect, ui.scope_id().with(name), Sense::CLICK).on_hover_cursor(CursorIcon::PointingHand)
}

fn top_bar(painter: &Painter, screen: Rect, games: usize, status: &Status) {
    let bar = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_BAR));
    painter.rect_filled(bar, 0.0, SURFACE);
    painter.rect_filled(Rect::from_min_max(pos2(bar.left(), bar.bottom() - BORDER_WIDTH), bar.max), 0.0, BORDER);
    let centre_y = bar.center().y;

    let logo = Rect::from_center_size(pos2(bar.left() + PADDING + 14.0, centre_y), vec2(28.0, 28.0));
    painter.rect_filled(logo, 7.0, ACCENT);
    icons::disc(painter, logo.center(), 6.0, ON_ACCENT);
    let title = painter.text(pos2(logo.right() + 16.0, centre_y), Align2::LEFT_CENTER, "Biblioteca", semibold(18.0), TEXT);
    let count = if games == 1 { "1 jogo".to_string() } else { format!("{games} jogos") };
    painter.text(pos2(title.right() + 16.0, centre_y + 1.0), Align2::LEFT_CENTER, count, regular(14.0), TEXT_SECONDARY);

    let connected = status.controllers > 0;
    let keyboard = chip(painter, bar.right() - PADDING, centre_y, "Teclado", true, TEXT_SECONDARY);
    let (label, colour) = if connected { ("Controle · Porta 1", TEXT) } else { ("Nenhum controle", TEXT_SECONDARY) };
    chip(painter, keyboard - 16.0, centre_y, label, connected, colour);
}

/// A pill with a status light, ending at `right`. Returns its left edge.
fn chip(painter: &Painter, right: f32, centre_y: f32, label: &str, on: bool, colour: Color32) -> f32 {
    const HEIGHT: f32 = 34.0;
    let text = painter.layout_no_wrap(label.to_owned(), regular(14.0), colour);
    let rect = Rect::from_min_max(pos2(right - text.size().x - 42.0, centre_y - HEIGHT / 2.0), pos2(right, centre_y + HEIGHT / 2.0));
    painter.rect_filled(rect, HEIGHT / 2.0, SURFACE2);
    icons::outline(painter, rect, HEIGHT / 2.0, BORDER_WIDTH, BORDER);
    icons::status(painter, pos2(rect.left() + 17.0, centre_y), on, OK, TEXT_SECONDARY);
    line(painter, rect.left() + 29.0, centre_y, text);
    rect.left()
}

fn footer(painter: &Painter, screen: Rect, empty: bool) {
    let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - FOOTER), screen.max);
    painter.rect_filled(bar, 0.0, SURFACE);
    painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width(), BORDER_WIDTH)), 0.0, BORDER);
    let centre_y = bar.center().y;

    // What the controller does, from the left.
    #[derive(Clone, Copy)]
    enum Icon {
        Face(Face),
        DPad,
    }
    let hints: &[(Icon, &str, Color32)] = if empty {
        &[(Icon::Face(Face::Bottom), "Selecionar", TEXT), (Icon::DPad, "Navegar", TEXT_SECONDARY)]
    } else {
        &[
            (Icon::Face(Face::Bottom), "Jogar", TEXT),
            (Icon::Face(Face::Right), "Detalhes", TEXT_SECONDARY),
            (Icon::DPad, "Navegar", TEXT_SECONDARY),
        ]
    };
    let mut x = bar.left() + PADDING;
    for (icon, label, colour) in hints {
        let centre = pos2(x + 9.0, centre_y);
        match icon {
            Icon::Face(face) => icons::face_buttons(painter, centre, *face, TEXT, TEXT_SECONDARY),
            Icon::DPad => icons::d_pad(painter, centre, TEXT),
        }
        let text = painter.text(pos2(x + 26.0, centre_y), Align2::LEFT_CENTER, label, regular(14.0), *colour);
        x = text.right() + PADDING;
    }

    // What the keyboard does, from the right.
    let label = painter.text(pos2(bar.right() - PADDING, centre_y), Align2::RIGHT_CENTER, "Teclado", regular(14.0), TEXT_SECONDARY);
    let second = key_cap(painter, label.left() - 8.0, centre_y, if empty { Some("Tab") } else { None });
    key_cap(painter, second - 6.0, centre_y, Some("Enter"));
}

/// A key cap ending at `right`, with a key's name or (without one) the
/// up and down arrows. Returns its left edge.
fn key_cap(painter: &Painter, right: f32, centre_y: f32, name: Option<&str>) -> f32 {
    const HEIGHT: f32 = 22.0;
    let text = name.map(|name| painter.layout_no_wrap(name.to_owned(), mono(12.0), TEXT));
    let width = text.as_ref().map_or(26.0, |text| text.size().x + 14.0);
    let rect = Rect::from_min_max(pos2(right - width, centre_y - HEIGHT / 2.0), pos2(right, centre_y + HEIGHT / 2.0));
    painter.rect_filled(rect, 5.0, BG);
    icons::outline(painter, rect, 5.0, BORDER_WIDTH, BORDER);
    match text {
        Some(text) => line(painter, rect.left() + 7.0, centre_y, text),
        None => icons::up_down(painter, rect.center(), TEXT),
    }
    rect.left()
}

/// The rows of the list that fit in a column `height` tall.
fn rows_that_fit(height: f32) -> usize {
    (((height + ROW_GAP) / (ROW_HEIGHT + ROW_GAP)).floor().max(1.0)) as usize
}

fn library(ui: &Ui, painter: &Painter, body: Rect, menu: &mut Menu, status: &Status) -> Option<Action> {
    let ctx = ui.ctx();
    let mut action = None;
    let list_right = if menu.details { body.right() - DETAILS_WIDTH - PADDING } else { body.right() };

    // The folder (click to pick another) and the order of the list.
    let folder = menu.folder.as_deref().map(|folder| display_path(folder, status.home.as_deref())).unwrap_or_default();
    let header_y = body.top() + 14.0;
    let folder_text = elided(painter, &folder, mono(13.0), TEXT_SECONDARY, list_right - body.left() - 120.0);
    let folder_rect = Rect::from_min_size(pos2(body.left(), header_y - 12.0), vec2(folder_text.size().x, 24.0));
    let folder_click = clickable(ui, folder_rect, "folder");
    if folder_click.clicked() {
        action = Some(Action::PickFolder);
    }
    let folder_colour = if folder_click.hovered() { TEXT } else { TEXT_SECONDARY };
    line(painter, body.left(), header_y, elided(painter, &folder, mono(13.0), folder_colour, list_right - body.left() - 120.0));
    painter.text(pos2(list_right, header_y), Align2::RIGHT_CENTER, "Ordem: nome", regular(13.0), TEXT_SECONDARY);

    // Why the last choice could not start, under the list.
    let list_top = body.top() + LIST_HEADER;
    let mut list_bottom = body.bottom();
    if let Some(message) = &menu.message {
        list_bottom -= MESSAGE_HEIGHT + ROW_GAP;
        let rect = Rect::from_min_max(pos2(body.left(), body.bottom() - MESSAGE_HEIGHT), pos2(list_right, body.bottom()));
        painter.rect_filled(rect, RADIUS, SURFACE);
        icons::outline(painter, rect, RADIUS, BORDER_WIDTH, BORDER);
        let mut job = LayoutJob::simple(message.clone(), regular(13.0), ERROR, rect.width() - 2.0 * ROW_INSET);
        job.wrap.max_rows = 2;
        let text = painter.fonts_mut(|fonts| fonts.layout_job(job));
        line(painter, rect.left() + ROW_INSET, rect.center().y, text);
    }

    menu.set_rows(rows_that_fit(list_bottom - list_top));
    let shown = menu.rows.min(menu.entries() - menu.top);
    for row in 0..shown {
        let index = menu.top + row;
        let rect = Rect::from_min_max(
            pos2(body.left(), list_top + row as f32 * (ROW_HEIGHT + ROW_GAP)),
            pos2(list_right, list_top + row as f32 * (ROW_HEIGHT + ROW_GAP) + ROW_HEIGHT),
        );
        let click = clickable(ui, rect, ("row", index));
        if click.clicked() {
            // A click selects; a click on the selected entry plays it.
            if index == menu.selected {
                action = Some(menu.play(index));
            } else {
                menu.select(index);
            }
        }
        let selected = index == menu.selected;
        let glow = ctx.animate_bool(ui.scope_id().with(("selected", index)), selected);
        let hover = if click.hovered() && !selected { SURFACE } else { BG };
        painter.rect_filled(rect, RADIUS, mix(hover, SURFACE2, glow));
        if glow > 0.0 {
            icons::outline(painter, rect, RADIUS, BORDER_WIDTH, mix(hover, ACCENT, glow));
        }
        row_contents(painter, rect, menu, index);
    }

    // A bar in the margin when there is more than fits.
    if menu.entries() > menu.rows {
        let track = Rect::from_min_max(pos2(list_right + 9.0, list_top), pos2(list_right + 13.0, list_bottom));
        painter.rect_filled(track, 2.0, SURFACE);
        let entries = menu.entries() as f32;
        let thumb = Rect::from_min_max(
            pos2(track.left(), track.top() + track.height() * menu.top as f32 / entries),
            pos2(track.right(), track.top() + track.height() * (menu.top + menu.rows) as f32 / entries),
        );
        painter.rect_filled(thumb, 2.0, BORDER);
    }

    if menu.details {
        let panel = Rect::from_min_max(pos2(body.right() - DETAILS_WIDTH, body.top()), body.max);
        if details(ui, painter, panel, menu, status) {
            action = Some(menu.play(menu.selected));
        }
    }
    action
}

/// The colour of the tile of the game at `index`.
fn tile_colour(index: usize) -> Color32 {
    TILES[index % TILES.len()]
}

/// The number on the tile of the game at `index`: its place in the list.
fn tile_number(index: usize) -> String {
    format!("{:02}", index + 1)
}

fn row_contents(painter: &Painter, rect: Rect, menu: &Menu, index: usize) {
    let tile = Rect::from_min_size(pos2(rect.left() + ROW_INSET, rect.center().y - TILE / 2.0), vec2(TILE, TILE));
    let text_left = tile.right() + ROW_INSET;
    let right = rect.right() - ROW_INSET;
    let (title, subtitle) = match menu.games.get(index) {
        Some(game) => {
            painter.rect_filled(tile, 8.0, tile_colour(index));
            painter.text(tile.center(), Align2::CENTER_CENTER, tile_number(index), semibold(14.0), ON_ACCENT);
            let size = painter.text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, format_size(game.size), mono(13.0), TEXT_SECONDARY);
            ((game.title.as_str(), size.left() - 16.0), game.subtitle())
        }
        None => {
            painter.rect_filled(tile, 8.0, SURFACE2);
            icons::outline(painter, tile, 8.0, BORDER_WIDTH, BORDER);
            icons::disc(painter, tile.center(), 8.0, TEXT_SECONDARY);
            (("Sem disco", right), "Shell do BIOS".to_string())
        }
    };
    let (title, title_right) = title;
    line(painter, text_left, rect.top() + 21.0, elided(painter, title, medium(16.0), TEXT, title_right - text_left));
    line(painter, text_left, rect.top() + 40.0, elided(painter, &subtitle, regular(13.0), TEXT_SECONDARY, title_right - text_left));
}

/// The panel about the selected entry. Returns whether "Jogar" was clicked.
fn details(ui: &Ui, painter: &Painter, panel: Rect, menu: &Menu, status: &Status) -> bool {
    painter.rect_filled(panel, CARD_RADIUS, SURFACE);
    icons::outline(painter, panel, CARD_RADIUS, BORDER_WIDTH, BORDER);
    let inner = panel.shrink(PANEL_INSET);
    let game = menu.games.get(menu.selected);

    let cover = Rect::from_min_size(inner.min, vec2(inner.width(), COVER_HEIGHT));
    match game {
        Some(_) => {
            painter.rect_filled(cover, RADIUS, tile_colour(menu.selected));
            painter.text(cover.center(), Align2::CENTER_CENTER, tile_number(menu.selected), semibold(56.0), ON_ACCENT);
        }
        None => {
            painter.rect_filled(cover, RADIUS, SURFACE2);
            icons::disc(painter, cover.center(), 26.0, TEXT_SECONDARY);
        }
    }

    let dash = || "—".to_string();
    let (title, path) = match game {
        Some(game) => (game.title.as_str(), display_path(&game.path, status.home.as_deref())),
        None => ("Sem disco", "Shell do BIOS".to_string()),
    };
    line(painter, inner.left(), cover.bottom() + 28.0, elided(painter, title, semibold(20.0), TEXT, inner.width()));
    line(painter, inner.left(), cover.bottom() + 51.0, elided(painter, &path, mono(12.5), TEXT_SECONDARY, inner.width()));

    let column = inner.width() / 2.0 + 6.0;
    let facts = [
        ("Formato", game.map_or_else(dash, |game| game.format.clone())),
        ("Região", game.and_then(|game| game.region).map_or_else(dash, |region| region.label().to_string())),
        ("Tamanho", game.map_or_else(dash, |game| format_size(game.size))),
        ("Memory card", if status.memory_card { "Slot 1".to_string() } else { dash() }),
    ];
    for (place, (label, value)) in facts.iter().enumerate() {
        let left = inner.left() + (place % 2) as f32 * column;
        let top = cover.bottom() + 84.0 + (place / 2) as f32 * 46.0;
        painter.text(pos2(left, top), Align2::LEFT_CENTER, label, regular(13.0), TEXT_SECONDARY);
        line(painter, left, top + 18.0, elided(painter, value, regular(15.0), TEXT, column - 12.0));
    }

    let button = Rect::from_min_max(pos2(inner.left(), inner.bottom() - BUTTON_HEIGHT), inner.max);
    let click = clickable(ui, button, "play");
    painter.rect_filled(button, RADIUS, if click.hovered() { mix(ACCENT, TEXT, 0.15) } else { ACCENT });
    let label = painter.layout_no_wrap("Jogar".to_owned(), semibold(15.0), ON_ACCENT);
    let left = button.center().x - (label.size().x + 22.0) / 2.0;
    icons::play(painter, pos2(left + 5.0, button.center().y), 10.0, ON_ACCENT);
    line(painter, left + 22.0, button.center().y, label);
    click.clicked()
}

/// The card of the screen without games, centred in `middle`.
fn empty_card(middle: Rect) -> Rect {
    Rect::from_center_size(middle.center(), EMPTY_CARD)
}

/// The two buttons of the screen without games, inside its card.
fn empty_buttons(card: Rect) -> [(EmptyButton, Rect); 2] {
    let left = card.left() + 36.0;
    let width = card.width() - 72.0;
    [
        (EmptyButton::PickFolder, Rect::from_min_size(pos2(left, card.top() + 271.0), vec2(width, BUTTON_HEIGHT))),
        (EmptyButton::OpenDisc, Rect::from_min_size(pos2(left, card.top() + 327.0), vec2(width, 44.0))),
    ]
}

fn empty_screen(ui: &Ui, painter: &Painter, middle: Rect, menu: &mut Menu, status: &Status) -> Option<Action> {
    let ctx = ui.ctx();
    let card = empty_card(middle);
    let centre_x = card.center().x;
    painter.rect_filled(card, CARD_RADIUS, SURFACE);
    icons::outline(painter, card, CARD_RADIUS, BORDER_WIDTH, BORDER);

    let icon = Rect::from_center_size(pos2(centre_x, card.top() + 73.0), vec2(64.0, 64.0));
    painter.rect_filled(icon, CARD_RADIUS, SURFACE2);
    icons::folder(painter, Rect::from_center_size(icon.center(), vec2(24.0, 19.0)), TEXT_SECONDARY);

    painter.text(pos2(centre_x, card.top() + 138.0), Align2::CENTER_CENTER, "Nenhum jogo encontrado", semibold(22.0), TEXT);
    let explanation = "Escolha a pasta com seus arquivos .cue, .bin, .iso ou .img. Ela fica salva para as próximas vezes.";
    centred(painter, centre_x, card.top() + 158.0, explanation, regular(14.5), TEXT_SECONDARY, card.width() - 72.0);

    // The folder that was looked in.
    let folder = match &menu.folder {
        Some(folder) => format!("{} (vazia)", display_path(folder, status.home.as_deref())),
        None => "Nenhuma pasta escolhida".to_string(),
    };
    let folder = elided(painter, &folder, mono(13.0), TEXT_SECONDARY, card.width() - 96.0);
    let pill = Rect::from_center_size(pos2(centre_x, card.top() + 236.0), vec2(folder.size().x + 22.0, 29.0));
    painter.rect_filled(pill, 6.0, BG);
    icons::outline(painter, pill, 6.0, BORDER_WIDTH, BORDER);
    line(painter, pill.left() + 11.0, pill.center().y, folder);

    let mut action = None;
    for (button, rect) in empty_buttons(card) {
        let click = clickable(ui, rect, ("empty", button == EmptyButton::PickFolder));
        if click.hovered() {
            menu.focus = button;
        }
        if click.clicked() {
            menu.message = None;
            action = Some(match button {
                EmptyButton::PickFolder => Action::PickFolder,
                EmptyButton::OpenDisc => Action::OpenDisc,
            });
        }
        let focus = ctx.animate_bool(ui.scope_id().with(("focus", button == EmptyButton::PickFolder)), menu.focus == button);
        match button {
            EmptyButton::PickFolder => {
                // Already the accent colour: the selection is a contour
                // around it, in the colour of the text.
                if focus > 0.0 {
                    icons::outline(painter, rect.expand(3.0), RADIUS + 3.0, BORDER_WIDTH, mix(SURFACE, TEXT, focus));
                }
                painter.rect_filled(rect, RADIUS, ACCENT);
                painter.text(rect.center(), Align2::CENTER_CENTER, "Escolher pasta", semibold(15.0), ON_ACCENT);
            }
            EmptyButton::OpenDisc => {
                painter.rect_filled(rect, RADIUS, mix(SURFACE, SURFACE2, focus));
                icons::outline(painter, rect, RADIUS, BORDER_WIDTH, mix(BORDER, ACCENT, focus));
                painter.text(rect.center(), Align2::CENTER_CENTER, "Abrir um disco avulso", medium(15.0), TEXT);
            }
        }
    }

    if let Some(message) = &menu.message {
        centred(painter, centre_x, card.bottom() + 16.0, message, regular(13.0), ERROR, middle.width() - 2.0 * PADDING);
    }
    action
}

#[cfg(test)]
mod tests {
    use super::super::menu::tests::games;
    use super::super::theme;
    use super::*;
    use eframe::egui;
    use egui_kittest::Harness;
    use psx_core::sio::button;

    #[test]
    fn the_design_window_fits_eight_rows() {
        let body = theme::WINDOW_HEIGHT - TOP_BAR - FOOTER - 2.0 * PADDING;
        assert_eq!(rows_that_fit(body - LIST_HEADER), 8);
        // One more needs a whole row and its gap.
        assert_eq!(rows_that_fit(8.0 * ROW_HEIGHT + 7.0 * ROW_GAP), 8);
        assert_eq!(rows_that_fit(9.0 * ROW_HEIGHT + 8.0 * ROW_GAP - 1.0), 8);
        assert_eq!(rows_that_fit(9.0 * ROW_HEIGHT + 8.0 * ROW_GAP), 9);
        assert_eq!(rows_that_fit(10.0), 1, "there is always one row");
    }

    #[test]
    fn tiles_are_numbered_from_one_and_cycle_through_the_colours() {
        assert_eq!(tile_number(0), "01");
        assert_eq!(tile_number(11), "12");
        assert_eq!(tile_number(123), "124");
        assert_eq!(tile_colour(0), TILES[0]);
        assert_eq!(tile_colour(TILES.len() + 2), TILES[2]);
    }

    #[test]
    fn the_empty_card_is_centred_with_its_buttons_inside() {
        let middle = Rect::from_min_max(pos2(0.0, TOP_BAR), pos2(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT - FOOTER));
        let card = empty_card(middle);
        assert_eq!((card.left(), card.top()), (292.0, 186.0), "where the mockup has it");
        let [(first, pick), (second, open)] = empty_buttons(card);
        assert_eq!((first, second), (EmptyButton::PickFolder, EmptyButton::OpenDisc));
        assert!(card.contains_rect(pick) && card.contains_rect(open));
        assert!(pick.bottom() < open.top());
    }

    /// The window at the design size, with the theme installed, as a picture.
    /// `None` on a machine without a graphics adapter to render with.
    fn render(menu: &mut Menu, status: &Status, name: &str) -> Option<image::RgbaImage> {
        let mut harness = Harness::builder()
            .with_size(vec2(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT))
            .with_pixels_per_point(1.0)
            .with_context_setup(theme::install)
            .wgpu()
            .build_ui(|ui| {
                show(ui, menu, status);
            });
        // Long enough for the selection animation to end.
        harness.run_steps(40);
        let picture = match harness.render() {
            Ok(picture) => picture,
            Err(error) => {
                eprintln!("skipping the rendered checks: {error}");
                return None;
            }
        };
        // YU_UI_SHOTS=<folder> keeps the pictures, to compare with the mockup.
        if let Some(folder) = std::env::var_os("YU_UI_SHOTS") {
            picture.save(std::path::Path::new(&folder).join(format!("{name}.png"))).unwrap();
        }
        Some(picture)
    }

    fn colour_at(picture: &image::RgbaImage, x: f32, y: f32) -> Color32 {
        let [r, g, b, _] = picture.get_pixel(x as u32, y as u32).0;
        Color32::from_rgb(r, g, b)
    }

    /// Whether any pixel of the area has (nearly) the colour.
    fn has_colour(picture: &image::RgbaImage, area: Rect, colour: Color32) -> bool {
        let near = |a: u8, b: u8| a.abs_diff(b) <= 2;
        (area.top() as u32..area.bottom() as u32).any(|y| {
            (area.left() as u32..area.right() as u32).any(|x| {
                let [r, g, b, _] = picture.get_pixel(x, y).0;
                near(r, colour.r()) && near(g, colour.g()) && near(b, colour.b())
            })
        })
    }

    fn status() -> Status {
        Status { controllers: 1, memory_card: true, home: Some(PathBuf::from("/home/someone")) }
    }

    #[test]
    fn the_library_is_drawn_like_the_mockup() {
        let mut menu = Menu::new(games(8), Some(PathBuf::from("/home/someone/Jogos")));
        menu.update(0);
        menu.update(button::DOWN);
        let Some(picture) = render(&mut menu, &status(), "biblioteca") else { return };
        assert_eq!(picture.dimensions(), (1024, 768));

        // Bars, background and panel, away from any text.
        assert_eq!(colour_at(&picture, 512.0, 10.0), SURFACE, "top bar");
        assert_eq!(colour_at(&picture, 512.0, TOP_BAR - 1.0), BORDER, "line under the top bar");
        assert_eq!(colour_at(&picture, 512.0, TOP_BAR + 2.0), BG);
        assert_eq!(colour_at(&picture, 512.0, 768.0 - FOOTER), BORDER, "line over the footer");
        assert_eq!(colour_at(&picture, 512.0, 768.0 - 4.0), SURFACE, "footer");
        assert_eq!(colour_at(&picture, 1024.0 - PADDING - 8.0, 500.0), SURFACE, "details panel");
        assert_eq!(colour_at(&picture, 1024.0 - PADDING - 0.5, 500.0), BORDER, "its border");
        assert_eq!(colour_at(&picture, 1024.0 - PADDING - DETAILS_WIDTH - 4.0, 500.0), BG, "gap before the panel");

        // The second row is selected: surface 2 with an accent contour.
        let row_top = |row: usize| TOP_BAR + PADDING + LIST_HEADER + row as f32 * (ROW_HEIGHT + ROW_GAP);
        let list_right = 1024.0 - PADDING - DETAILS_WIDTH - PADDING;
        assert_eq!(colour_at(&picture, 400.0, row_top(1) + 30.0), SURFACE2);
        assert_eq!(colour_at(&picture, 400.0, row_top(1) + 0.5), ACCENT);
        assert_eq!(colour_at(&picture, 400.0, row_top(1) + ROW_HEIGHT - 0.5), ACCENT);
        assert_eq!(colour_at(&picture, list_right - 0.5, row_top(1) + 30.0), ACCENT);
        assert_eq!(colour_at(&picture, 400.0, row_top(0) + 30.0), BG, "the others have no background");
        assert_eq!(colour_at(&picture, 400.0, row_top(0) + 0.5), BG);
        assert_eq!(colour_at(&picture, 400.0, row_top(1) + ROW_HEIGHT + 2.0), BG, "gap between rows");

        // Each game has its tile; the cover and the button follow the selection.
        for (row, tile) in TILES.iter().enumerate() {
            assert_eq!(colour_at(&picture, PADDING + ROW_INSET + 3.0, row_top(row) + 14.0), *tile, "tile {row}");
        }
        let panel_left = 1024.0 - PADDING - DETAILS_WIDTH + PANEL_INSET;
        assert_eq!(colour_at(&picture, panel_left + 10.0, TOP_BAR + PADDING + PANEL_INSET + 10.0), TILES[1], "cover");
        assert_eq!(colour_at(&picture, panel_left + 10.0, 768.0 - FOOTER - PADDING - PANEL_INSET - 10.0), ACCENT, "play button");

        // Text is there, in the colours of the theme, and the status lights are on.
        let area = |left: f32, top: f32, right: f32, bottom: f32| Rect::from_min_max(pos2(left, top), pos2(right, bottom));
        assert!(has_colour(&picture, area(60.0, 16.0, 170.0, 48.0), TEXT), "title");
        assert!(has_colour(&picture, area(88.0, row_top(3), 300.0, row_top(3) + 30.0), TEXT), "game title");
        assert!(has_colour(&picture, area(88.0, row_top(3) + 30.0, 300.0, row_top(3) + 56.0), TEXT_SECONDARY), "format and region");
        assert!(has_colour(&picture, area(560.0, row_top(3), list_right, row_top(3) + ROW_HEIGHT), TEXT_SECONDARY), "size");
        assert!(has_colour(&picture, area(700.0, 16.0, 1000.0, 48.0), OK), "status lights");
        assert!(!has_colour(&picture, area(0.0, 0.0, 1024.0, 768.0), ERROR), "no message");
    }

    #[test]
    fn the_library_without_details_uses_the_whole_width() {
        let mut menu = Menu::new(games(3), Some(PathBuf::from("/home/someone/Jogos")));
        menu.update(0);
        menu.update(button::CIRCLE);
        menu.message = Some("could not read /home/someone/Jogos/Jogo 01.cue: no such file".to_string());
        let Some(picture) = render(&mut menu, &Status::default(), "biblioteca-sem-detalhes") else { return };
        let row_top = TOP_BAR + PADDING + LIST_HEADER;
        assert_eq!(colour_at(&picture, 1024.0 - PADDING - 0.5, row_top + 30.0), ACCENT, "the row reaches the right margin");
        assert_eq!(colour_at(&picture, 900.0, 500.0), BG, "no panel");
        // The message is under the list, and nothing is green but the keyboard's light.
        let area = |left: f32, top: f32, right: f32, bottom: f32| Rect::from_min_max(pos2(left, top), pos2(right, bottom));
        assert!(has_colour(&picture, area(PADDING, 768.0 - FOOTER - PADDING - MESSAGE_HEIGHT, 1000.0, 768.0 - FOOTER - PADDING), ERROR));
        assert!(has_colour(&picture, area(900.0, 16.0, 1000.0, 48.0), OK), "keyboard");
        assert!(!has_colour(&picture, area(700.0, 16.0, 900.0, 48.0), OK), "no controller");
    }

    #[test]
    fn the_screen_without_games_offers_a_folder_and_a_single_disc() {
        let mut menu = Menu::new(Vec::new(), Some(PathBuf::from("/home/someone/Jogos")));
        menu.update(0);
        let Some(picture) = render(&mut menu, &Status::default(), "pasta-sem-jogos") else { return };
        let middle = Rect::from_min_max(pos2(0.0, TOP_BAR), pos2(1024.0, 768.0 - FOOTER));
        let card = empty_card(middle);
        assert_eq!(colour_at(&picture, card.left() + 20.0, card.top() + 20.0), SURFACE, "card");
        assert_eq!(colour_at(&picture, card.left() + 0.5, card.center().y), BORDER, "its border");
        assert_eq!(colour_at(&picture, card.left() - 20.0, card.center().y), BG);
        let [(_, pick), (_, open)] = empty_buttons(card);
        assert_eq!(colour_at(&picture, pick.left() + 12.0, pick.center().y), ACCENT, "pick a folder");
        assert_eq!(colour_at(&picture, open.left() + 12.0, open.center().y), SURFACE, "open a disc: not selected");
        assert_eq!(colour_at(&picture, open.left() + 12.0, open.top() + 0.5), BORDER);
        assert!(has_colour(&picture, pick.shrink(8.0), ON_ACCENT), "label on the accent colour");
        assert!(has_colour(&picture, open.shrink(8.0), TEXT));

        // Down moves the selection to the second button.
        menu.update(button::DOWN);
        let Some(picture) = render(&mut menu, &Status::default(), "pasta-sem-jogos-disco") else { return };
        assert_eq!(colour_at(&picture, open.left() + 12.0, open.center().y), SURFACE2);
        assert_eq!(colour_at(&picture, open.left() + 12.0, open.top() + 0.5), ACCENT);
    }

    #[test]
    fn a_click_selects_a_row_and_a_second_click_plays_it() {
        let mut menu = Menu::new(games(4), None);
        let mut actions = Vec::new();
        {
            let mut harness = Harness::builder()
                .with_size(vec2(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT))
                .with_context_setup(theme::install)
                .build_ui(|ui| actions.extend(show(ui, &mut menu, &Status::default())));
            let third_row = pos2(300.0, TOP_BAR + PADDING + LIST_HEADER + 2.0 * (ROW_HEIGHT + ROW_GAP) + 30.0);
            let click = |harness: &mut Harness<'_>| {
                for pressed in [true, false] {
                    harness.event(egui::Event::PointerButton {
                        pos: third_row,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    });
                    harness.step();
                }
            };
            harness.hover_at(third_row);
            harness.step();
            click(&mut harness);
            click(&mut harness);
        }
        assert_eq!(menu.selected, 2);
        assert_eq!(actions, [Action::Play(super::super::menu::Choice::Game(PathBuf::from("g2.cue")))]);
    }
}
