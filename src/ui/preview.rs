//! Deterministic visual checks using the real Slint software renderer.
//! Set SONADIAL_RENDER_PREVIEW to an output directory to write PPM frames and
//! geometry.json, without opening the player or changing its saved state.

use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use std::io::Write;

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

pub(super) fn render(dir: &Path) -> anyhow::Result<()> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone())))?;
    register_fonts();
    let ui = Main::new()?;
    let style = ui.global::<Style>();
    let look = theme::builtin_look(1);
    theme::set_look(look);
    let colors = ui.global::<Theme>();
    colors.set_c_bg(rgba(look.palette.bg, 255));
    colors.set_c_bg_deep(rgba(look.palette.bg_deep, 255));
    colors.set_c_surface(rgba(look.palette.surface, 255));
    colors.set_c_line(rgba(look.palette.line, 255));
    colors.set_c_text_hi(rgba(look.palette.text_hi, 255));
    colors.set_c_text(rgba(look.palette.text, 255));
    colors.set_c_text_dim(rgba(look.palette.text_dim, 255));
    colors.set_c_danger(rgba(look.palette.danger, 255));
    ui.set_tracks(slint::ModelRc::new(slint::VecModel::from(
        [
            "Good Times - Rynn",
            "中文歌曲：夜空中最亮的星",
            "サカナクション - 多分、風。",
            "한국어 노래 / Fusion 12",
        ]
        .iter()
        .enumerate()
        .map(|(index, title)| TrackRow {
            index: index as i32,
            title: (*title).into(),
            meta: "FLAC".into(),
            ..Default::default()
        })
        .collect::<Vec<_>>(),
    )));
    ui.set_track_count(4);
    ui.set_dock_title("中文歌曲 / サカナクション / 한국어 / Fusion 12".into());
    ui.set_dock_title_live(true);
    ui.set_lyric_current("中文歌词 / サカナクション / 한국어 / Pixel 24".into());
    ui.set_lyric_next("下一句歌词 / 次の歌詞 / 다음 가사 / Pixel 12".into());
    ui.set_lyric_active(true);
    ui.set_time_text("0:41 / 5:00".into());
    ui.set_core_status("[PLAYING]".into());
    ui.set_play_label("PAUSE".into());
    ui.set_seek_cells(60);
    ui.set_seek_filled(8);
    ui.set_seek_filled_str("#".repeat(8).into());
    ui.set_seek_rest_str("-".repeat(52).into());
    let volume_cells = style.get_dock_volume_cells() as usize;
    let volume_filled = (volume_cells as f32 * 0.62).round() as usize;
    ui.set_vol_filled(volume_filled as i32);
    ui.set_vol_fill_str("#".repeat(volume_filled).into());
    ui.set_vol_rest_str("-".repeat(volume_cells - volume_filled).into());
    ui.set_out_label("PREVIEW OUTPUT".into());
    ui.set_download_out_dir("Music / SonadialPlayer".into());
    ui.set_confirm_name("中文歌曲：夜空中最亮的星.flac".into());
    ui.set_confirm_path("Music / 中文歌曲：夜空中最亮的星.flac".into());
    ui.set_error_text("Preview notification / 中文提示".into());
    ui.set_eq_bands(slint::ModelRc::new(slint::VecModel::from(
        BAND_FREQS.iter().enumerate().map(|(i, freq)| EqBandData {
            gain: (i as f32 - 4.0) * 0.5,
            label: format!("{:+.1}", (i as f32 - 4.0) * 0.5).into(),
            freq: freq.to_string().into(),
        }).collect::<Vec<_>>(),
    )));
    let chip = |index: usize| {
        let look = theme::builtin_look(index);
        ChipData {
            name: format!("THEME {index:02}").into(),
            grad_a: rgba(look.palette.bg, 255),
            grad_b: rgba(look.palette.surface, 255),
            fg: rgba(look.palette.text_hi, 255),
            dim: rgba(look.palette.text_dim, 255),
            index: index as i32,
            col: (index % style.get_theme_columns() as usize) as i32,
            row: (index / style.get_theme_columns() as usize) as i32,
            ..Default::default()
        }
    };
    ui.set_auto_chips(slint::ModelRc::new(slint::VecModel::from(vec![chip(0), chip(1)])));
    ui.set_theme_groups(slint::ModelRc::new(slint::VecModel::from(vec![ChipGroup {
        label: "PREVIEW THEMES".into(),
        rows: 2,
        chips: slint::ModelRc::new(slint::VecModel::from((0..8).map(chip).collect::<Vec<_>>())),
    }])));
    ui.set_theme_content_h(style.get_theme_auto_label_h() + style.get_chip_h()
        + style.get_theme_gap() * 2.0 + style.get_theme_group_label_h()
        + style.get_theme_row_step() * 2.0);
    let weak = ui.as_weak();
    ui.on_select_track(move |index| {
        if let Some(ui) = weak.upgrade() {
            ui.set_selected_index(index);
        }
    });
    let interactions = Rc::new(RefCell::new(Vec::<serde_json::Value>::new()));
    let samples = interactions.clone();
    ui.on_seek(move |value| {
        samples.borrow_mut().push(serde_json::json!({"control": "seek", "value": value}));
    });
    let samples = interactions.clone();
    ui.on_set_volume(move |value| {
        samples.borrow_mut().push(serde_json::json!({"control": "volume", "value": value}));
    });
    let samples = interactions.clone();
    ui.on_prev_track(move || samples.borrow_mut().push(serde_json::json!({"control": "prev"})));
    let samples = interactions.clone();
    ui.on_toggle_play(move || samples.borrow_mut().push(serde_json::json!({"control": "play"})));
    let samples = interactions.clone();
    ui.on_stop(move || samples.borrow_mut().push(serde_json::json!({"control": "stop"})));
    let samples = interactions.clone();
    ui.on_next_track(move || samples.borrow_mut().push(serde_json::json!({"control": "next"})));
    ui.show()?;

    let bars: Vec<f32> = (0..crate::spectrum::NUM_BARS)
        .map(|i| {
            let t = i as f32 / crate::spectrum::NUM_BARS as f32;
            crate::spectrum::MAX_DB_VALUE * (0.15 + 0.65 * (t * 9.0).cos().powi(4))
        })
        .collect();
    let peaks: Vec<f32> = bars
        .iter()
        .map(|v| (v + 6.0).min(crate::spectrum::MAX_DB_VALUE))
        .collect();
    let mut stage = Stage::new();
    let mut geometry = Vec::new();
    std::fs::create_dir_all(dir)?;
    let normal_w = style.get_window_w() as u32;
    let normal_h = style.get_window_h() as u32;
    let minimum_w = style.get_window_min_w() as u32;
    let minimum_h = style.get_window_min_h() as u32;
    // Reuse one window and Stage so resizing, restoring, and changing styles
    // also exercise layout invalidation and the cached background's key.
    for (name, width, height, dots, sidebar, show_bg) in [
        ("dots-normal", normal_w, normal_h, true, true, true),
        ("dots-maximized", 1918, 1032, true, true, true),
        ("dots-restored", normal_w, normal_h, true, true, true),
        ("default", normal_w, normal_h, false, true, true),
        ("dots-returned", normal_w, normal_h, true, true, true),
        ("dots-no-background", normal_w, normal_h, true, true, false),
        ("dots-no-sidebar", normal_w, normal_h, true, false, true),
        ("dots-minimum", minimum_w, minimum_h, true, true, true),
        ("dots-hover", normal_w, normal_h, true, true, true),
        ("dots-selected", normal_w, normal_h, true, true, true),
        ("dots-hover-cleared", normal_w, normal_h, true, true, true),
        ("dock-hover", normal_w, normal_h, true, true, true),
        ("settings-system", normal_w, normal_h, true, true, true),
        ("settings-visuals", normal_w, normal_h, true, true, true),
        ("settings-theme", normal_w, normal_h, true, true, true),
        ("eq", normal_w, normal_h, true, true, true),
        ("download", normal_w, normal_h, true, true, true),
        ("context", normal_w, normal_h, true, true, true),
        ("confirm", normal_w, normal_h, true, true, true),
        ("toast", normal_w, normal_h, true, true, true),
    ] {
        window.set_size(slint::PhysicalSize::new(width, height));
        ui.set_window_maximized(name == "dots-maximized");
        ui.set_sidebar_open(sidebar);
        ui.set_visual_style(i32::from(dots));
        ui.set_selected_index(-1);
        ui.set_show_settings(name.starts_with("settings-"));
        ui.set_settings_page(match name {
            "settings-visuals" => 1,
            "settings-theme" => 2,
            _ => 0,
        });
        ui.set_show_eq(name == "eq");
        ui.set_show_download(name == "download");
        ui.set_confirm_visible(name == "confirm");
        ui.set_error_visible(name == "toast");
        ui.set_context_index(if name == "context" { 1 } else { -1 });
        ui.set_context_title("中文歌曲：夜空中最亮的星".into());
        ui.set_context_x(style.get_library_inset());
        ui.set_context_y(style.get_library_list_top() + style.get_row_h());
        ui.set_context_has_lrc(true);
        window.dispatch_event(WindowEvent::PointerExited);
        if name == "dock-hover" {
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(
                    width as f32 / 2.0 - style.get_dock_transport_w() / 2.0
                        + style.get_dock_button_w() * 1.5 + style.get_dock_button_gap(),
                    height as f32 - style.get_dock_h() + style.get_dock_controls_y()
                        + style.get_dock_controls_h() / 2.0,
                ),
            });
        }
        if name == "dots-hover" || name == "dots-selected" {
            let position = slint::LogicalPosition::new(150.0, 186.0);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            if name == "dots-selected" {
                window.dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
                window.dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                });
                window.dispatch_event(WindowEvent::PointerExited);
            }
        }
        let rect = (
            ui.get_stage_x(),
            ui.get_stage_y(),
            ui.get_stage_w(),
            ui.get_stage_h(),
        );
        let output = stage.paint(
            width,
            height,
            1.0,
            &look,
            rect,
            StageInput {
                bars: &bars,
                peaks: &peaks,
                visual: VisualState {
                    kick: 0.4,
                    ..Default::default()
                },
                playing: true,
                pulse_enabled: true,
                show_bg_spectrum: show_bg,
                show_radial: true,
                peak_hold: true,
                rotation: 0.17,
                radial_scale: 1.0,
                led_bars: dots,
                dot_style: dots,
                braille_stream: dots,
                beat_age: 0.16,
                ring_strength: 0.7,
            },
        );
        ui.set_disc_r(output.disc_r);
        ui.set_stage(slint::Image::from_rgba8(output.buffer));
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        ui.window().request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(pixels.make_mut_slice(), width as usize);
        });
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(dir.join(format!("{name}.ppm")))?);
        write!(file, "P6\n{width} {height}\n255\n")?;
        file.write_all(pixels.as_bytes())?;
        geometry.push(serde_json::json!({
            "name": name, "width": width, "height": height,
            "stage": [rect.0, rect.1, rect.2, rect.3], "disc_radius": output.disc_r,
            "list_height": ui.get_library_viewport_h(),
            "dock_height": style.get_dock_h(),
        }));
    }
    std::fs::write(
        dir.join("geometry.json"),
        serde_json::to_vec_pretty(&geometry)?,
    )?;
    ui.set_error_visible(false);
    // Dispatch real pointer events at the two meters' endpoints and midpoint.
    // Python checks these recorded callback values, rather than duplicating the
    // component's hit-testing formula in a Rust assertion test.
    for (prefix, cells, y) in [
        (1.0, ui.get_seek_cells() as f32, style.get_dock_seek_y() + style.get_dock_seek_h() / 2.0),
        (5.0, style.get_dock_volume_cells() as f32, style.get_dock_controls_y() + style.get_dock_controls_h() / 2.0),
    ] {
        for fraction in [0.0, 0.5, 1.0] {
            let position = slint::LogicalPosition::new(
                style.get_dock_inset() + style.get_dock_cell_w() * (prefix + fraction * cells),
                normal_h as f32 - style.get_dock_h() + y,
            );
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
        }
    }
    let mut button_x = (normal_w as f32 - style.get_dock_transport_w()) / 2.0;
    for width in [
        style.get_dock_button_w(), style.get_dock_button_w(),
        style.get_dock_stop_w(), style.get_dock_button_w(),
    ] {
        let position = slint::LogicalPosition::new(
            button_x + width / 2.0,
            normal_h as f32 - style.get_dock_h() + style.get_dock_controls_y()
                + style.get_dock_controls_h() / 2.0,
        );
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        button_x += width + style.get_dock_button_gap();
    }
    std::fs::write(
        dir.join("interactions.json"),
        serde_json::to_vec_pretty(&*interactions.borrow())?,
    )?;
    Ok(())
}
