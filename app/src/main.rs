mod nmea;

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use nmea::GpsData;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const FIELD_HIGHLIGHT_DURATION: Duration = Duration::from_secs(2);

fn main() -> eframe::Result {
    eframe::run_native(
        "Pico GPS",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 720.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(GpsApp::default()))),
    )
}

struct GpsApp {
    endpoint: String,
    connected: bool,
    service_connected: bool,
    pending: Option<Receiver<Result<String, String>>>,
    next_poll: Instant,
    request_error: Option<String>,
    data: GpsData,
}

impl Default for GpsApp {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            connected: false,
            service_connected: false,
            pending: None,
            next_poll: Instant::now(),
            request_error: None,
            data: GpsData::default(),
        }
    }
}

impl GpsApp {
    fn disconnect(&mut self) {
        self.connected = false;
        self.service_connected = false;
        self.pending = None;
        self.request_error = None;
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    self.next_poll = Instant::now() + POLL_INTERVAL;
                    match result {
                        Ok(text) => {
                            self.service_connected = true;
                            self.request_error = None;
                            if !text.is_empty() {
                                self.data.push(&text);
                            }
                        }
                        Err(error) => {
                            self.service_connected = false;
                            self.request_error = Some(error);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.service_connected = false;
                    self.request_error = Some("The HTTP worker stopped unexpectedly.".into());
                    self.next_poll = Instant::now() + POLL_INTERVAL;
                }
            }
        }

        if self.connected && self.pending.is_none() && Instant::now() >= self.next_poll {
            let endpoint = self.endpoint.trim().to_owned();
            let (sender, receiver) = mpsc::channel();
            let repaint = ctx.clone();
            self.pending = Some(receiver);
            std::thread::spawn(move || {
                let result = fetch_data(&endpoint);
                // Disconnecting deliberately drops the receiver.
                let _ = sender.send(result);
                repaint.request_repaint();
            });
        }
        if self.connected {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn fetch_data(endpoint: &str) -> Result<String, String> {
    use std::io::Read;

    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| error.to_string())?
        .get(endpoint)
        .header(reqwest::header::CACHE_CONTROL, "no-store")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    let mut text = String::new();
    response
        .take(65_537)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() > 65_536 {
        return Err("The /data response exceeded 64 KiB.".into());
    }
    Ok(text)
}

impl eframe::App for GpsApp {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);

        egui::TopBottomPanel::top("connection").show(ctx, |ui| {
            ui.heading("Pico GPS");
            ui.horizontal(|ui| {
                ui.label("Data endpoint");
                ui.add_enabled(
                    !self.connected,
                    egui::TextEdit::singleline(&mut self.endpoint)
                        .hint_text("http://<pico-ip>/data")
                        .desired_width(420.0),
                );
                if self.connected {
                    if ui.button("Disconnect").clicked() {
                        self.disconnect();
                    }
                } else if ui.button("Connect").clicked() {
                    match reqwest::Url::parse(self.endpoint.trim()) {
                        Ok(url)
                            if matches!(url.scheme(), "http" | "https") && url.host().is_some() =>
                        {
                            self.connected = true;
                            self.service_connected = false;
                            self.data = GpsData::default();
                            self.request_error = None;
                            self.next_poll = Instant::now();
                        }
                        _ => {
                            self.request_error = Some(
                                "Enter a valid HTTP or HTTPS URL for the Pico's /data endpoint."
                                    .into(),
                            );
                        }
                    }
                }
                let color = if self.service_connected {
                    egui::Color32::GREEN
                } else {
                    egui::Color32::RED
                };
                let indicator = ui.allocate_response(egui::vec2(16.0, 16.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(indicator.rect.center(), 6.0, color);
                let mut details = if self.service_connected {
                    "Connected"
                } else if self.connected {
                    "Connecting / retrying"
                } else {
                    "Disconnected"
                }
                .to_owned();
                if let Some(error) = &self.request_error {
                    details.push_str(&format!("\n{error}"));
                }
                if let Some(error) = &self.data.error {
                    details.push_str(&format!("\nGPS data error: {error}"));
                }
                indicator.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &details)
                });
                indicator.on_hover_text(details);
            });
        });

        show_data_panels(ctx, &self.data);
    }
}

fn show_data_panels(ctx: &egui::Context, data: &GpsData) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let reserved_bottom = (ui.available_height() * 0.35).min(160.0);
        let max_sections_height = (ui.available_height() - reserved_bottom).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("gps_sections_scroll")
            .max_height(max_sections_height)
            .min_scrolled_height(0.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                show_sentences(ui, data);
            });
        ui.separator();
        ui.columns(2, |columns| {
            let ui = &mut columns[0];
            ui.label("Raw GPS stream");
            egui::ScrollArea::both()
                .id_salt("raw_stream_scroll")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let mut raw = data.raw_stream.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut raw)
                            .code_editor()
                            .desired_rows(1)
                            .desired_width(f32::INFINITY),
                    );
                });
            egui::ScrollArea::vertical()
                .id_salt("position_scroll")
                .show(&mut columns[1], |ui| show_position(ui, &data.position));
        });
    });
}

fn show_position(ui: &mut egui::Ui, position: &nmea::Position) {
    ui.label("Position (decimal degrees)");
    let coordinates = position.coordinates;
    let latitude = coordinates
        .map(|value| format!("{:.6}", value.latitude))
        .unwrap_or_default();
    let longitude = coordinates
        .map(|value| format!("{:.6}", value.longitude))
        .unwrap_or_default();
    let width = (ui.available_width() - 90.0).max(40.0);
    egui::Grid::new("decimal_position")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("Latitude");
            readonly_value(ui, &latitude, width, position.changed_at[0]);
            ui.end_row();
            ui.label("Longitude");
            readonly_value(ui, &longitude, width, position.changed_at[1]);
            ui.end_row();
        });
    if let Some(error) = &position.error {
        ui.colored_label(egui::Color32::LIGHT_RED, error);
    } else if coordinates.is_none() {
        ui.label("No GPS fix");
    }
    if let Some(source) = &position.source {
        ui.label(format!("Source: {source}"));
    }
}

fn show_sentences(ui: &mut egui::Ui, data: &GpsData) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
        let spacing = ui.spacing().item_spacing.x;
        let available = ui.available_width();
        let columns = ((available + spacing) / (420.0 + spacing)).floor().max(1.0) as usize;
        let section_width = (available - spacing * (columns - 1) as f32) / columns as f32;
        let fields_width = section_width;
        let label_width = (fields_width - spacing) * 0.55;
        let value_width = (fields_width - spacing) * 0.45;

        ui.columns(columns, |columns| {
            for (key, sentence) in data
                .sentences
                .iter()
                .filter(|(_, sentence)| sentence.kind != "GSV")
            {
                let ui = shortest_column(columns);
                ui.label(key);
                egui::Grid::new(key)
                    .num_columns(2)
                    .spacing(egui::vec2(4.0, 2.0))
                    .max_col_width(label_width)
                    .striped(true)
                    .show(ui, |ui| {
                        for (index, value) in sentence.fields.iter().enumerate() {
                            ui.add_sized(
                                [label_width, 0.0],
                                egui::Label::new(nmea::field_label(
                                    &sentence.kind,
                                    index,
                                    sentence.fields.len(),
                                ))
                                .wrap(),
                            );
                            readonly_value(ui, value, value_width, sentence.changed_at[index]);
                            ui.end_row();
                        }
                        ui.add_sized([label_width, 0.0], egui::Label::new("Raw sentence").wrap());
                        readonly_value(ui, &sentence.raw, value_width, sentence.raw_changed_at);
                        ui.end_row();
                    });
            }
            if data
                .sentences
                .values()
                .any(|sentence| sentence.kind == "GSV")
            {
                show_satellites(shortest_column(columns), data);
            }
        });
    });
}

fn shortest_column(columns: &mut [egui::Ui]) -> &mut egui::Ui {
    columns
        .iter_mut()
        .min_by(|left, right| {
            left.next_widget_position()
                .y
                .total_cmp(&right.next_widget_position().y)
        })
        .expect("GPS layout always has at least one column")
}

fn show_satellites(ui: &mut egui::Ui, data: &GpsData) {
    ui.label("GSV satellites");
    egui::ScrollArea::both()
        .id_salt("gsv_satellites_scroll")
        .min_scrolled_height(0.0)
        .max_height(180.0)
        .show(ui, |ui| {
            for ((id, signal), pages) in data.satellite_groups() {
                ui.push_id((id, signal), |ui| {
                    ui.label(id);
                    let first = pages[0];
                    ui.horizontal(|ui| {
                        ui.label("Pages");
                        readonly_value(ui, &first.fields[0], 24.0, first.changed_at[0]);
                        ui.label("In view");
                        readonly_value(ui, &first.fields[2], 24.0, first.changed_at[2]);
                        if first.fields.len().is_multiple_of(4) {
                            ui.label("Signal");
                            readonly_value(
                                ui,
                                signal,
                                24.0,
                                first.changed_at[first.fields.len() - 1],
                            );
                        }
                    });
                    egui::Grid::new("satellites")
                        .num_columns(5)
                        .spacing(egui::vec2(4.0, 2.0))
                        .striped(true)
                        .show(ui, |ui| {
                            for heading in ["Page", "ID", "Elev.", "Azimuth", "SNR"] {
                                ui.label(heading);
                            }
                            ui.end_row();
                            for page in pages {
                                let end = if page.fields.len().is_multiple_of(4) {
                                    page.fields.len() - 1
                                } else {
                                    page.fields.len()
                                };
                                for (satellite, values) in
                                    page.fields[3..end].as_chunks::<4>().0.iter().enumerate()
                                {
                                    if values.iter().all(String::is_empty) {
                                        continue;
                                    }
                                    readonly_value(ui, &page.fields[1], 38.0, page.changed_at[1]);
                                    for (offset, value) in values.iter().enumerate() {
                                        readonly_value(
                                            ui,
                                            value,
                                            38.0,
                                            page.changed_at[3 + satellite * 4 + offset],
                                        );
                                    }
                                    ui.end_row();
                                }
                            }
                        });
                });
            }
        });
}

fn readonly_value(ui: &mut egui::Ui, value: &str, width: f32, changed_at: Option<Instant>) {
    let mut displayed = value.to_owned();
    let mut edit = egui::TextEdit::singleline(&mut displayed).desired_width(width);
    if let Some(remaining) =
        changed_at.and_then(|changed| FIELD_HIGHLIGHT_DURATION.checked_sub(changed.elapsed()))
    {
        let color = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(95, 75, 15)
        } else {
            egui::Color32::from_rgb(255, 235, 155)
        };
        edit = edit.background_color(color);
        ui.ctx().request_repaint_after(remaining);
    }
    ui.add_enabled(false, edit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn highlights_changed_fields_and_restores_the_original_background_after_expiry() {
        let backgrounds = |changed_at| {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    readonly_value(ui, "changed", 100.0, changed_at);
                });
            });
            output
                .shapes
                .into_iter()
                .filter_map(|clipped| {
                    if let egui::Shape::Rect(rect) = clipped.shape {
                        Some(rect.fill)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let normal = backgrounds(None);
        let highlighted = backgrounds(Some(Instant::now()));
        assert!(highlighted.iter().any(|color| !normal.contains(color)));
        assert_eq!(
            normal,
            backgrounds(Some(
                Instant::now() - FIELD_HIGHLIGHT_DURATION - Duration::from_secs(1)
            ))
        );
    }

    #[test]
    fn combines_satellite_pages_in_one_compact_section() {
        let mut data = GpsData::default();
        data.push("$GPGSV,2,1,05,01,40,083,42,1\n$GPGSV,2,2,05,02,30,090,35,1\n");
        let ctx = egui::Context::default();
        for frame in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 720.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_sentences(ui, &data);
                    });
                },
            );
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        Some((text.galley.text(), text.pos))
                    } else {
                        None
                    }
                })
                .collect();
            if frame < 2 {
                continue;
            }
            assert_eq!(
                labels
                    .iter()
                    .filter(|(label, _)| *label == "GSV satellites")
                    .count(),
                1
            );
            assert!(!labels.iter().any(|(label, _)| label.contains("/ page")));
            let first = labels.iter().find(|(label, _)| *label == "01").unwrap().1;
            let second = labels
                .iter()
                .find(|(label, _)| *label == "02")
                .unwrap_or_else(|| panic!("Missing second satellite row: {labels:?}"))
                .1;
            assert_eq!(first.x, second.x);
            assert!(second.y > first.y && second.y - first.y < 30.0);
        }
    }

    #[test]
    fn connection_indicator_tracks_service_responses_and_disconnects() {
        let ctx = egui::Context::default();
        let mut app = GpsApp {
            connected: true,
            ..Default::default()
        };
        assert!(!app.service_connected);
        let deliver = |app: &mut GpsApp, result| {
            let (sender, receiver) = mpsc::channel();
            sender.send(result).unwrap();
            app.pending = Some(receiver);
            app.poll(&ctx);
        };

        deliver(&mut app, Ok(String::new()));
        assert!(
            app.service_connected,
            "Empty successful responses are connected"
        );
        deliver(&mut app, Err("Connection refused".into()));
        assert!(!app.service_connected);
        assert!(app.connected, "Polling must continue after a failure");
        assert_eq!(app.request_error.as_deref(), Some("Connection refused"));
        deliver(&mut app, Ok("$GPGLL,value\n".into()));
        assert!(app.service_connected);
        assert!(app.request_error.is_none());

        let (sender, receiver) = mpsc::channel();
        app.pending = Some(receiver);
        app.poll(&ctx);
        assert!(
            app.service_connected,
            "An in-flight poll must not flicker red"
        );
        drop(sender);
        app.poll(&ctx);
        assert!(
            !app.service_connected,
            "A failed worker must show disconnected"
        );

        deliver(&mut app, Ok(String::new()));
        app.disconnect();
        assert!(!app.service_connected);
        assert!(!app.connected);
        assert!(app.pending.is_none());
        assert!(app.data.sentences.contains_key("GPGLL"));
    }

    #[test]
    fn arranges_sections_horizontally_and_wraps_on_narrow_windows() {
        let mut data = GpsData::default();
        data.push("$GPGLL,value\n$GPGSA,value\n$GPRMC,value\n");

        for width in [1400.0, 600.0] {
            let positions = section_positions(&data, width);
            assert_eq!(positions.len(), 3);
            let gll = positions["GPGLL"];
            let gsa = positions["GPGSA"];
            let rmc = positions["GPRMC"];
            if width > 1000.0 {
                assert_eq!(gll.y, gsa.y);
                assert_eq!(gsa.y, rmc.y);
                assert!(gll.x < gsa.x && gsa.x < rmc.x);
            } else {
                assert_eq!(gll.x, gsa.x);
                assert_eq!(gsa.x, rmc.x);
                assert!(gll.y < gsa.y && gsa.y < rmc.y);
            }
        }
    }

    #[test]
    fn packs_sections_under_shorter_neighbors_instead_of_waiting_for_a_row() {
        let mut data = GpsData::default();
        data.push("$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,\n");
        data.push("$GPGLL,value\n$GPGSA,value\n$GPRMC,value\n");
        let positions = section_positions(&data, 900.0);
        let gga = positions["GPGGA"];
        let gll = positions["GPGLL"];
        let gsa = positions["GPGSA"];
        let rmc = positions["GPRMC"];
        assert_eq!(gga.y, gll.y);
        assert_eq!(gll.x, gsa.x);
        assert_eq!(gll.x, rmc.x);
        assert!(gll.y < gsa.y && gsa.y < rmc.y);
        assert!(gsa.y - gll.y < 100.0);
    }

    #[test]
    fn packs_the_screenshot_sections_and_starts_the_stream_without_a_blank_band() {
        let mut data = GpsData::default();
        data.push(concat!(
            "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,\n",
            "$GPGLL,4807.038,N,01131.000,E,123519,A,A\n",
            "$GPGSA,A,3,04,05,09,12,,,,,,,,,1.8,1.0,1.2\n",
            "$GPRMC,123519,A,4807.038,N,01131.000,E,1.2,90.0,121126,,,A\n",
            "$GPVTG,90.0,T,,M,1.2,N,2.2,K,A\n",
            "$GPGSV,2,1,04,01,40,083,42,02,30,090,35,1\n",
            "$GPGSV,2,2,04,03,20,120,28,04,15,180,25,1\n",
        ));
        let ctx = egui::Context::default();
        for frame in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1800.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ctx| show_data_panels(ctx, &data),
            );
            if frame < 2 {
                continue;
            }
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect();
            let find = |label| {
                *text
                    .iter()
                    .find(|text| text.galley.text() == label)
                    .unwrap()
            };
            assert_eq!(find("GPVTG").pos.x, find("GPGLL").pos.x);
            assert!(find("GPVTG").pos.y < find("GPGSA").pos.y + 250.0);
            let stream_y = find("Raw GPS stream").pos.y;
            assert!(
                stream_y < 500.0,
                "The compact sections should leave more room for the stream"
            );
            let sections_bottom = text
                .iter()
                .filter(|text| text.pos.y < stream_y)
                .map(|text| text.pos.y + text.galley.size().y)
                .reduce(f32::max)
                .unwrap();
            assert!(
                stream_y - sections_bottom <= 20.0,
                "There should be no blank band above the stream"
            );
        }
    }

    #[test]
    fn reserves_a_separate_bottom_panel_for_the_raw_stream() {
        let mut data = GpsData::default();
        data.push(
            "$GPGLL,4807.038,N,01131.000,E,123519,A\n$GPRMC,123519,A,4807.038,N,01131.000,E\n",
        );
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 720.0),
                    )),
                    ..Default::default()
                },
                |ctx| show_data_panels(ctx, &data),
            );
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect();
            let heading = text
                .iter()
                .find(|text| text.galley.text() == "Raw GPS stream")
                .unwrap();
            assert!(
                heading.pos.y < 250.0,
                "Unused space should be assigned to the raw stream"
            );
            let position = text
                .iter()
                .find(|text| text.galley.text() == "Position (decimal degrees)")
                .unwrap();
            assert!(position.pos.x > heading.pos.x + 400.0);
            assert!((position.pos.y - heading.pos.y).abs() < 1.0);
            for value in ["48.117300", "11.516667"] {
                let coordinate = text
                    .iter()
                    .find(|text| text.galley.text() == value)
                    .unwrap();
                assert!(coordinate.pos.x > position.pos.x);
                assert!(coordinate.pos.y > position.pos.y);
            }
            for id in ["GPGLL", "GPRMC"] {
                let section = text.iter().find(|text| text.galley.text() == id).unwrap();
                assert!(section.pos.y < heading.pos.y);
            }
            assert!(
                text.iter()
                    .any(|text| text.galley.text() == data.raw_stream)
            );
        }
    }

    fn section_positions(
        data: &GpsData,
        width: f32,
    ) -> std::collections::BTreeMap<String, egui::Pos2> {
        let ctx = egui::Context::default();
        let mut positions = std::collections::BTreeMap::new();
        for _ in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 900.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let available = ui.available_width();
                        show_sentences(ui, data);
                        assert!(
                            ui.min_rect().width() <= available + 1.0,
                            "Sections overflow the available width: {} > {available}",
                            ui.min_rect().width()
                        );
                    });
                },
            );
            positions.clear();
            for clipped in output.shapes {
                if let egui::Shape::Text(text) = clipped.shape {
                    let label = text.galley.text();
                    if data.sentences.contains_key(label) {
                        positions.insert(label.to_owned(), text.pos);
                    }
                }
            }
        }
        assert_eq!(positions.len(), data.sentences.len());
        positions
    }

    fn serve_once(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/data", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (endpoint, worker)
    }

    #[test]
    fn fetches_data_and_reports_http_errors() {
        let body = "$GPRMC,123519,A,4807.038,N,01131.000,E\n";
        let (endpoint, worker) = serve_once("200 OK", body);
        assert_eq!(fetch_data(&endpoint).unwrap(), body);
        assert!(worker.join().unwrap().starts_with("GET /data HTTP/1.1\r\n"));

        let (endpoint, worker) = serve_once("503 Service Unavailable", "Unavailable");
        assert!(fetch_data(&endpoint).unwrap_err().contains("503"));
        worker.join().unwrap();
    }

    #[test]
    fn enforces_response_size_limit_at_the_boundary() {
        for size in [65_536, 65_537] {
            let (endpoint, worker) = serve_once("200 OK", &"x".repeat(size));
            let result = fetch_data(&endpoint);
            if size == 65_536 {
                assert_eq!(result.unwrap().len(), size);
            } else {
                assert!(result.unwrap_err().contains("exceeded 64 KiB"));
            }
            worker.join().unwrap();
        }
    }
}
