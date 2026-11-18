use std::collections::BTreeMap;
use std::time::Instant;

const RAW_STREAM_LIMIT: usize = 20_000;

#[derive(Default)]
pub struct GpsData {
    pub sentences: BTreeMap<String, Sentence>,
    pub raw_stream: String,
    pub position: Position,
    pub error: Option<String>,
    pending: String,
}

pub struct Sentence {
    pub id: String,
    pub kind: String,
    pub fields: Vec<String>,
    pub raw: String,
    pub changed_at: Vec<Option<Instant>>,
    pub raw_changed_at: Option<Instant>,
}

impl Sentence {
    pub fn signal_id(&self) -> Option<&str> {
        if self.kind == "GSV" && self.fields.len().is_multiple_of(4) {
            self.fields.last().map(String::as_str)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Coordinates {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Default)]
pub struct Position {
    pub coordinates: Option<Coordinates>,
    pub source: Option<String>,
    pub error: Option<String>,
    pub changed_at: [Option<Instant>; 2],
}

impl Position {
    fn update(&mut self, sentence: &Sentence, now: Instant) {
        let result = sentence_coordinates(sentence);
        let (coordinates, error) = match result {
            Ok(coordinates) => (coordinates, None),
            Err(error) => (None, Some(error)),
        };
        let old_values = self
            .coordinates
            .map(|value| [value.latitude, value.longitude]);
        let new_values = coordinates.map(|value| [value.latitude, value.longitude]);
        for index in 0..2 {
            if self.source.is_some()
                && old_values.map(|values| values[index]) != new_values.map(|values| values[index])
            {
                self.changed_at[index] = Some(now);
            }
        }
        self.coordinates = coordinates;
        self.source = Some(sentence.id.clone());
        self.error = error;
    }
}

impl GpsData {
    pub fn push(&mut self, text: &str) {
        self.raw_stream.push_str(text);
        if self.raw_stream.len() > RAW_STREAM_LIMIT {
            let mut start = self.raw_stream.len() - RAW_STREAM_LIMIT;
            while !self.raw_stream.is_char_boundary(start) {
                start += 1;
            }
            self.raw_stream.drain(..start);
        }
        self.pending.push_str(text);
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_sentence(line) {
                Ok((key, mut sentence)) => {
                    let now = Instant::now();
                    if let Some(previous) = self.sentences.get(&key) {
                        for (index, value) in sentence.fields.iter().enumerate() {
                            sentence.changed_at[index] =
                                if previous.fields.get(index) == Some(value) {
                                    previous.changed_at[index]
                                } else {
                                    Some(now)
                                };
                        }
                        sentence.raw_changed_at = if previous.raw == sentence.raw {
                            previous.raw_changed_at
                        } else {
                            Some(now)
                        };
                    }
                    if matches!(sentence.kind.as_str(), "GGA" | "GLL" | "RMC") {
                        self.position.update(&sentence, now);
                    }
                    if sentence.kind == "GSV" {
                        let total = sentence.fields[0]
                            .parse::<u16>()
                            .expect("validated GSV total");
                        self.sentences.retain(|_, previous| {
                            previous.kind != "GSV"
                                || previous.id != sentence.id
                                || previous.signal_id() != sentence.signal_id()
                                || previous.fields[1]
                                    .parse::<u16>()
                                    .expect("validated GSV page")
                                    <= total
                        });
                    }
                    self.sentences.insert(key, sentence);
                }
                Err(error) => self.error = Some(error),
            }
        }
        if self.pending.len() > 4096 {
            self.pending.clear();
            self.error = Some("Incomplete NMEA sentence exceeded 4096 bytes; discarded it.".into());
        }
    }

    pub fn satellite_groups(&self) -> BTreeMap<(&str, &str), Vec<&Sentence>> {
        let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for sentence in self
            .sentences
            .values()
            .filter(|sentence| sentence.kind == "GSV")
        {
            let signal = sentence.signal_id().unwrap_or("");
            groups
                .entry((sentence.id.as_str(), signal))
                .or_default()
                .push(sentence);
        }
        for pages in groups.values_mut() {
            pages.sort_by_key(|sentence| {
                sentence.fields[1]
                    .parse::<u16>()
                    .expect("validated GSV page")
            });
        }
        groups
    }
}

enum Axis {
    Latitude,
    Longitude,
}

fn decimal_degrees(value: &str, hemisphere: &str, axis: Axis) -> Result<f64, String> {
    let (digits, maximum, positive, negative, name) = match axis {
        Axis::Latitude => (4, 90.0, "N", "S", "latitude"),
        Axis::Longitude => (5, 180.0, "E", "W", "longitude"),
    };
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.len() != digits
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("Invalid {name} DDM value: {value:?}"));
    }
    let ddm = value
        .parse::<f64>()
        .map_err(|error| format!("Invalid {name}: {error}"))?;
    let degrees = (ddm / 100.0).floor();
    let minutes = ddm - degrees * 100.0;
    if !ddm.is_finite()
        || minutes >= 60.0
        || degrees > maximum
        || (degrees == maximum && minutes > 0.0)
    {
        return Err(format!("Out-of-range {name} DDM value: {value}"));
    }
    let decimal = degrees + minutes / 60.0;
    match hemisphere {
        direction if direction == positive => Ok(decimal),
        direction if direction == negative => Ok(-decimal),
        _ => Err(format!("Invalid {name} hemisphere: {hemisphere:?}")),
    }
}

fn sentence_coordinates(sentence: &Sentence) -> Result<Option<Coordinates>, String> {
    let field = |index: usize| {
        sentence
            .fields
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("Incomplete {} position fields", sentence.id))
    };
    let (latitude, longitude, valid) = match sentence.kind.as_str() {
        "GGA" => {
            let quality = field(5)?
                .parse::<u8>()
                .map_err(|error| format!("Invalid fix quality: {error}"))?;
            (1, 3, quality != 0)
        }
        "GLL" | "RMC" => {
            let (latitude, longitude, status, mode) = if sentence.kind == "GLL" {
                (0, 2, 5, 6)
            } else {
                (2, 4, 1, 11)
            };
            let valid = match field(status)? {
                "A" => true,
                "V" => false,
                status => return Err(format!("Invalid position status: {status:?}")),
            } && sentence.fields.get(mode).is_none_or(|mode| mode != "N");
            (latitude, longitude, valid)
        }
        _ => return Err(format!("{} does not contain a position", sentence.id)),
    };
    if !valid {
        return Ok(None);
    }
    Ok(Some(Coordinates {
        latitude: decimal_degrees(field(latitude)?, field(latitude + 1)?, Axis::Latitude)?,
        longitude: decimal_degrees(field(longitude)?, field(longitude + 1)?, Axis::Longitude)?,
    }))
}

fn parse_sentence(raw: &str) -> Result<(String, Sentence), String> {
    let body = raw
        .strip_prefix('$')
        .ok_or_else(|| format!("Expected a NMEA sentence starting with '$': {raw}"))?;
    let body = if let Some((body, checksum)) = body.split_once('*') {
        let expected = u8::from_str_radix(checksum, 16)
            .ok()
            .filter(|_| checksum.len() == 2);
        let actual = body.bytes().fold(0, |sum, byte| sum ^ byte);
        if expected != Some(actual) {
            return Err(format!("Invalid NMEA checksum: {raw}"));
        }
        body
    } else {
        body
    };
    let mut parts = body.split(',');
    let id = parts.next().unwrap_or_default();
    if id.len() < 5 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(format!("Invalid NMEA sentence identifier: {raw}"));
    }
    let kind = id[id.len() - 3..].to_owned();
    let fields: Vec<String> = parts.map(str::to_owned).collect();
    if kind == "GSV" {
        let count = fields.len();
        let valid_pages = fields
            .first()
            .and_then(|value| value.parse::<u16>().ok())
            .zip(fields.get(1).and_then(|value| value.parse::<u16>().ok()))
            .is_some_and(|(total, page)| total > 0 && page > 0 && page <= total);
        if count < 3
            || (!((count - 3).is_multiple_of(4)) && !(count >= 4 && (count - 4).is_multiple_of(4)))
            || !valid_pages
            || fields[2].parse::<u16>().is_err()
        {
            return Err(format!("Invalid GSV page: {raw}"));
        }
    }
    // GSV satellite lists span multiple numbered sentences. Keep every page
    // and signal ID instead of overwriting all but the last one.
    let key = if kind == "GSV" {
        let page = fields.get(1).map(String::as_str).unwrap_or("");
        let signal = if fields.len().is_multiple_of(4) {
            fields.last().map(String::as_str).unwrap_or("")
        } else {
            ""
        };
        format!("{id} / page {page} / signal {signal}")
    } else if kind == "GSA" && fields.len() > 17 {
        format!("{id} / system {}", fields[17])
    } else {
        id.to_owned()
    };
    Ok((
        key,
        Sentence {
            id: id.to_owned(),
            kind,
            changed_at: vec![None; fields.len()],
            fields,
            raw: raw.to_owned(),
            raw_changed_at: None,
        },
    ))
}

pub fn field_label(kind: &str, index: usize, field_count: usize) -> String {
    if kind == "GSV" && field_count.is_multiple_of(4) && index + 1 == field_count {
        return "Signal ID".into();
    }
    let labels: &[&str] = match kind {
        "GGA" => &[
            "UTC time (hhmmss.ss)",
            "Latitude (ddmm.mmmm)",
            "North / south",
            "Longitude (dddmm.mmmm)",
            "East / west",
            "Fix quality",
            "Satellites used",
            "Horizontal dilution (HDOP)",
            "Altitude",
            "Altitude units",
            "Geoid separation",
            "Geoid separation units",
            "Differential correction age (s)",
            "Differential station ID",
        ],
        "RMC" => &[
            "UTC time (hhmmss.ss)",
            "Status (A = valid, V = void)",
            "Latitude (ddmm.mmmm)",
            "North / south",
            "Longitude (dddmm.mmmm)",
            "East / west",
            "Speed over ground (knots)",
            "Course over ground (degrees)",
            "Date (ddmmyy)",
            "Magnetic variation (degrees)",
            "Variation east / west",
            "Mode indicator",
            "Navigation status",
        ],
        "GLL" => &[
            "Latitude (ddmm.mmmm)",
            "North / south",
            "Longitude (dddmm.mmmm)",
            "East / west",
            "UTC time (hhmmss.ss)",
            "Status",
            "Mode indicator",
        ],
        "VTG" => &[
            "True course (degrees)",
            "True course reference",
            "Magnetic course (degrees)",
            "Magnetic course reference",
            "Speed (knots)",
            "Knots unit",
            "Speed (km/h)",
            "Kilometres per hour unit",
            "Mode indicator",
        ],
        "GSA" => {
            return match index {
                0 => "Selection mode (M = manual, A = automatic)".into(),
                1 => "Fix type (1 = none, 2 = 2D, 3 = 3D)".into(),
                2..=13 => format!("Satellite {} ID", index - 1),
                14 => "Position dilution (PDOP)".into(),
                15 => "Horizontal dilution (HDOP)".into(),
                16 => "Vertical dilution (VDOP)".into(),
                17 => "GNSS system ID".into(),
                _ => format!("Field {}", index + 1),
            };
        }
        "GSV" => {
            return match index {
                0 => "Total messages".into(),
                1 => "Message number".into(),
                2 => "Satellites in view".into(),
                _ => {
                    let property = [
                        "ID",
                        "Elevation (degrees)",
                        "Azimuth (degrees)",
                        "SNR (dB-Hz)",
                    ][(index - 3) % 4];
                    format!("Satellite {} {property}", (index - 3) / 4 + 1)
                }
            };
        }
        "ZDA" => &[
            "UTC time (hhmmss.ss)",
            "Day",
            "Month",
            "Year",
            "Local zone hours",
            "Local zone minutes",
        ],
        "GST" => &[
            "UTC time (hhmmss.ss)",
            "Range RMS deviation (m)",
            "Error ellipse semi-major deviation (m)",
            "Error ellipse semi-minor deviation (m)",
            "Error ellipse orientation (degrees)",
            "Latitude deviation (m)",
            "Longitude deviation (m)",
            "Altitude deviation (m)",
        ],
        _ => &[],
    };
    labels
        .get(index)
        .map(|label| (*label).to_owned())
        .unwrap_or_else(|| format!("Field {}", index + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_changed_fields_and_keeps_highlights_on_repeated_values() {
        let mut data = GpsData::default();
        data.push("$GPXYZ,first,stable\n");
        assert!(
            data.sentences["GPXYZ"]
                .changed_at
                .iter()
                .all(Option::is_none)
        );
        data.push("$GPXYZ,second,stable\n");
        let changed = data.sentences["GPXYZ"].changed_at[0];
        assert!(changed.is_some());
        assert!(data.sentences["GPXYZ"].changed_at[1].is_none());
        assert!(data.sentences["GPXYZ"].raw_changed_at.is_some());
        data.push("$GPXYZ,second,stable\n");
        assert_eq!(data.sentences["GPXYZ"].changed_at[0], changed);
        data.push("$GPXYZ,second,stable,new\n");
        assert!(data.sentences["GPXYZ"].changed_at[2].is_some());
    }

    #[test]
    fn converts_ddm_with_hemispheres_and_validates_coordinate_ranges() {
        for (value, hemisphere, axis, expected) in [
            ("4807.038", "N", Axis::Latitude, 48.1173),
            ("4807.038", "S", Axis::Latitude, -48.1173),
            ("01131.000", "E", Axis::Longitude, 11.5166666667),
            ("01131.000", "W", Axis::Longitude, -11.5166666667),
            ("9000.000", "N", Axis::Latitude, 90.0),
            ("18000.000", "W", Axis::Longitude, -180.0),
            ("0000.000", "S", Axis::Latitude, 0.0),
        ] {
            assert!((decimal_degrees(value, hemisphere, axis).unwrap() - expected).abs() < 1e-9);
        }
        for (value, hemisphere, axis) in [
            ("4860.000", "N", Axis::Latitude),
            ("9000.001", "N", Axis::Latitude),
            ("18000.001", "E", Axis::Longitude),
            ("18100.000", "W", Axis::Longitude),
            ("-4807.038", "S", Axis::Latitude),
            ("4807.038", "E", Axis::Latitude),
            ("4807.038", "", Axis::Latitude),
            ("NaN", "N", Axis::Latitude),
            ("4807.0e1", "N", Axis::Latitude),
            ("", "N", Axis::Latitude),
        ] {
            assert!(decimal_degrees(value, hemisphere, axis).is_err());
        }
    }

    #[test]
    fn updates_position_from_gll_gga_rmc_and_clears_an_invalid_fix() {
        let mut data = GpsData::default();
        data.push("$GPGLL,4807.038,N,01131.000,E,123519,A\n");
        let coordinates = data.position.coordinates.unwrap();
        assert!((coordinates.latitude - 48.1173).abs() < 1e-9);
        assert!(data.position.changed_at.iter().all(Option::is_none));
        data.push("$GPGGA,123519,4807.0380,N,01131.000,E,1\n");
        assert_eq!(data.position.source.as_deref(), Some("GPGGA"));
        assert!(data.position.changed_at.iter().all(Option::is_none));
        data.push("$GPRMC,123519,A,4807.038,N,01131.000,W\n");
        assert!(data.position.coordinates.unwrap().longitude < 0.0);
        assert!(data.position.changed_at[0].is_none());
        assert!(data.position.changed_at[1].is_some());
        data.push("$GPGGA,123519,4807.038,N,01131.000,W,0\n");
        assert!(data.position.coordinates.is_none());
        assert!(data.position.error.is_none());
        data.push("$GPGLL,4860.000,N,01131.000,E,123519,A\n");
        assert!(data.position.coordinates.is_none());
        assert!(
            data.position
                .error
                .as_deref()
                .unwrap()
                .contains("Out-of-range latitude")
        );
        data.push("$GPRMC,123519,V,4807.038,N,01131.000,E\n");
        assert!(data.position.coordinates.is_none());
        assert!(data.position.error.is_none());
    }

    #[test]
    fn retains_raw_chunks_and_bounds_history_without_splitting_utf8() {
        let mut data = GpsData::default();
        data.push("noise\r\n$GPGLL,par");
        data.push("tial\n");
        assert_eq!(data.raw_stream, "noise\r\n$GPGLL,partial\n");
        assert!(data.error.is_some());
        data.push(&"\u{e9}".repeat(RAW_STREAM_LIMIT));
        data.push("$GPGLL,latest\n");
        assert!(data.raw_stream.len() <= RAW_STREAM_LIMIT);
        assert!(data.raw_stream.starts_with('\u{e9}'));
        assert!(data.raw_stream.ends_with("$GPGLL,latest\n"));
    }

    #[test]
    fn assembles_chunks_and_preserves_empty_and_unknown_fields() {
        let mut data = GpsData::default();
        data.push("$GPGGA,123519,4807.");
        assert!(data.sentences.is_empty());
        data.push("038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n$GPXYZ,a,,c,\n");
        assert!(data.error.is_none());
        let gga = &data.sentences["GPGGA"];
        assert_eq!(gga.fields.len(), 14);
        assert_eq!(gga.fields[1], "4807.038");
        assert_eq!(gga.fields[13], "");
        assert_eq!(data.sentences["GPXYZ"].fields, ["a", "", "c", ""]);
        assert_eq!(field_label("XYZ", 3, 4), "Field 4");
    }

    #[test]
    fn rejects_corruption_without_replacing_last_valid_values() {
        let mut data = GpsData::default();
        data.push("$GPRMC,first\n$GPRMC,corrupt*00\nnot NMEA\n");
        assert_eq!(data.sentences["GPRMC"].fields, ["first"]);
        assert!(data.error.is_some());
        data.push(&"x".repeat(4097));
        assert!(data.pending.is_empty());
        data.push("$GPRMC,recovered\n");
        assert_eq!(data.sentences["GPRMC"].fields, ["recovered"]);
    }

    #[test]
    fn retains_satellite_pages_signals_and_constellations() {
        let mut data = GpsData::default();
        data.push("$GPGSV,2,1,05,01,40,083,42,1\n$GPGSV,2,2,05,02,30,090,35,1\n");
        data.push("$GPGSV,2,1,05,01,40,083,42,2\n$GLGSV,1,1,01,65,20,100,30\n");
        assert_eq!(data.sentences.len(), 4);
        data.push("$GPGSV,2,1,05,01,45,083,43,1\n");
        assert_eq!(data.sentences.len(), 4);
        assert_eq!(data.sentences["GPGSV / page 1 / signal 1"].fields[4], "45");
        let groups = data.satellite_groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[&("GPGSV", "1")].len(), 2);
        assert_eq!(groups[&("GPGSV", "1")][1].fields[1], "2");
        assert_eq!(groups[&("GLGSV", "")][0].fields[3], "65");
        data.push("$GPGSV,1,2,01,01,40,083,42\n");
        assert!(data.error.as_deref().unwrap().contains("Invalid GSV page"));
        data.push("$GPGSV,1,1,01,01,40,083,42,1\n");
        let groups = data.satellite_groups();
        assert_eq!(groups[&("GPGSV", "1")].len(), 1);
        assert_eq!(groups[&("GPGSV", "2")].len(), 1);
    }
}
