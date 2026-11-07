//! Date values live in Heap's side map. UTC arithmetic covers all of TimeClip;
//! chrono supplies the clock and the host's local timezone, without JS callbacks.
use super::heap::{Heap, HeapId, JsNumber, VmValue};
use super::js_value;
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};

const LIMIT: f64 = 8_640_000_000_000_000.0;
const DAY: i64 = 86_400_000;

fn number(value: f64) -> VmValue {
    VmValue::Number(JsNumber::new(value))
}

fn clip(ms: f64) -> f64 {
    if !ms.is_finite() || ms.abs() > LIMIT {
        f64::NAN
    } else {
        ms.trunc() + 0.0
    }
}

// Proleptic Gregorian arithmetic, including astronomical year zero. Chrono's
// NaiveDate range is smaller than ECMAScript's +/-100,000,000 days.
fn days_from_civil(year: i64, month: i64, date: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let d = (153 * m + 2) / 5 + date - 1;
    era * 146_097 + y * 365 + y / 4 - y / 100 + d - 719_468
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Parts {
    year: i64,
    month: u32,
    date: u32,
    day: u32,
    hours: u32,
    minutes: u32,
    seconds: u32,
    millis: u32,
}

fn parts(ms: f64) -> Option<Parts> {
    // Local offsets can move a valid instant slightly outside TimeClip.
    if !ms.is_finite() || ms.abs() > LIMIT + 3.0 * DAY as f64 {
        return None;
    }
    let ms = ms as i64;
    let days = ms.div_euclid(DAY);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = ms.rem_euclid(DAY);
    Some(Parts {
        year,
        month: month as u32,
        date: date as u32,
        day: (days + 4).rem_euclid(7) as u32,
        hours: (time / 3_600_000) as u32,
        minutes: (time / 60_000 % 60) as u32,
        seconds: (time / 1000 % 60) as u32,
        millis: (time % 1000) as u32,
    })
}

fn naive(p: Parts, year: i64) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, p.month, p.date)?
        .and_hms_milli_opt(p.hours, p.minutes, p.seconds, p.millis)
}

fn local_probe(ms: f64) -> Option<NaiveDateTime> {
    if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(ms as i64) {
        return Some(dt.naive_utc());
    }
    let p = parts(ms)?;
    // Outside chrono's calendar range, retain the same 400-year calendar and
    // historical/future era. Timezone rules there are necessarily extrapolated.
    let year = (if p.year < 0 { -260_000 } else { 260_000 }) + p.year.rem_euclid(400);
    naive(p, year)
}

fn offset_at(ms: f64) -> i32 {
    local_probe(ms)
        .map(|dt| Local.offset_from_utc_datetime(&dt).local_minus_utc())
        .unwrap_or(0)
}

fn earlier_offset(offset: LocalResult<i32>) -> Option<i32> {
    match offset {
        LocalResult::Single(offset) => Some(offset),
        // Larger local-minus-UTC yields the earlier instant in an overlap.
        LocalResult::Ambiguous(a, b) => Some(a.max(b)),
        LocalResult::None => None,
    }
}

fn resolve_local(
    wall: NaiveDateTime,
    lookup: impl Fn(NaiveDateTime) -> LocalResult<i32>,
) -> Option<f64> {
    let wall_ms = wall.and_utc().timestamp_millis() as f64;
    if let Some(offset) = earlier_offset(lookup(wall)) {
        return Some(wall_ms - offset as f64 * 1000.0);
    }
    // Node's compatible disambiguation advances through gaps using the offset
    // before the transition. Include half-hour DST and 24-hour date-line gaps.
    for step in 1..=3 * 24 * 4 {
        let prior = wall.checked_sub_signed(Duration::minutes(step * 15))?;
        if let Some(offset) = earlier_offset(lookup(prior)) {
            return Some(wall_ms - offset as f64 * 1000.0);
        }
    }
    None
}

fn local_to_utc(wall_ms: f64) -> f64 {
    let Some(_) = parts(wall_ms) else {
        return f64::NAN;
    };
    let Some(wall) = local_probe(wall_ms) else {
        return f64::NAN;
    };
    let shift = wall_ms - wall.and_utc().timestamp_millis() as f64;
    resolve_local(wall, |dt| {
        Local
            .offset_from_local_datetime(&dt)
            .map(|offset| offset.local_minus_utc())
    })
    .map(|utc| clip(utc + shift))
    .unwrap_or(f64::NAN)
}

fn components(heap: &Heap, args: &[VmValue], utc: bool) -> Result<f64> {
    let mut values = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (index, arg) in args.iter().take(7).enumerate() {
        values[index] = js_value::to_number(heap, arg)?.trunc();
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Ok(f64::NAN);
    }
    let [mut year, month, date, hours, minutes, seconds, millis] = values;
    if (0.0..=99.0).contains(&year) {
        year += 1900.0;
    }
    // These bounds also match V8 MakeDay's guards against huge intermediates;
    // large but useful day/time overflow is still normalized before TimeClip.
    if year.abs() > 1_000_000.0 || month.abs() > 10_000_000.0 {
        return Ok(f64::NAN);
    }
    let month = month as i64;
    let year = year as i64 + month.div_euclid(12);
    let day = days_from_civil(year, month.rem_euclid(12) + 1, 1) as f64 + date - 1.0;
    let time = hours * 3_600_000.0 + minutes * 60_000.0 + seconds * 1000.0 + millis;
    let wall = day * DAY as f64 + time;
    Ok(if utc { clip(wall) } else { local_to_utc(wall) })
}

pub(super) fn construct(heap: &mut Heap, args: &[VmValue]) -> Result<VmValue> {
    let ms = match args {
        [] => Utc::now().timestamp_millis() as f64,
        [value] => {
            if let VmValue::Reference(id) = value {
                if let Some(ms) = heap.date_value(*id) {
                    return heap.new_date(clip(ms));
                }
            }
            match js_value::to_primitive(heap, value)? {
                VmValue::String(text) => parse(&text),
                value => js_value::to_number(heap, &value)?,
            }
        }
        _ => components(heap, args, false)?,
    };
    heap.new_date(clip(ms))
}

pub(super) fn static_call(heap: &mut Heap, name: &str, args: &[VmValue]) -> Result<VmValue> {
    match name.strip_prefix("Date.").unwrap_or(name) {
        "Date" | "call" => Ok(VmValue::String(display(
            Utc::now().timestamp_millis() as f64
        ))),
        "now" => Ok(number(Utc::now().timestamp_millis() as f64)),
        "parse" => Ok(number(parse(&js_value::to_string(
            heap,
            args.first().unwrap_or(&VmValue::Undefined),
        )?))),
        "UTC" => Ok(number(components(heap, args, true)?)),
        _ => bail!("TypeError: Date.{name} is not a function"),
    }
}

pub(super) fn method(heap: &Heap, id: HeapId, name: &str, _args: &[VmValue]) -> Result<VmValue> {
    let Some(ms) = heap.date_value(id) else {
        bail!("TypeError: Date.{name} called on incompatible receiver")
    };
    let ms = clip(ms);
    match name {
        "getTime" | "valueOf" => return Ok(number(ms)),
        "toISOString" => return Ok(VmValue::String(iso(ms)?)),
        "toJSON" => {
            return Ok(if ms.is_finite() {
                VmValue::String(iso(ms)?)
            } else {
                VmValue::Null
            });
        }
        "toString" => return Ok(VmValue::String(display(ms))),
        _ => {}
    }
    let (field, utc) = if let Some(field) = name.strip_prefix("getUTC") {
        (field, true)
    } else if let Some(field) = name.strip_prefix("get") {
        (field, false)
    } else {
        bail!("TypeError: Date.{name} is not a function");
    };
    ensure!(
        matches!(
            field,
            "FullYear"
                | "Month"
                | "Date"
                | "Day"
                | "Hours"
                | "Minutes"
                | "Seconds"
                | "Milliseconds"
        ),
        "TypeError: Date.{name} is not a function"
    );
    if !ms.is_finite() {
        return Ok(number(f64::NAN));
    }
    let p = parts(
        ms + if utc {
            0.0
        } else {
            offset_at(ms) as f64 * 1000.0
        },
    )
    .unwrap();
    Ok(number(match field {
        "FullYear" => p.year as f64,
        "Month" => (p.month - 1) as f64,
        "Date" => p.date as f64,
        "Day" => p.day as f64,
        "Hours" => p.hours as f64,
        "Minutes" => p.minutes as f64,
        "Seconds" => p.seconds as f64,
        "Milliseconds" => p.millis as f64,
        _ => unreachable!(),
    }))
}

pub(super) fn iso(ms: f64) -> Result<String> {
    let Some(p) = parts(clip(ms)) else {
        bail!("RangeError: Invalid time value")
    };
    let year = if (0..=9999).contains(&p.year) {
        format!("{:04}", p.year)
    } else {
        format!("{:+07}", p.year)
    };
    Ok(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        p.month, p.date, p.hours, p.minutes, p.seconds, p.millis
    ))
}

pub(super) fn display(ms: f64) -> String {
    let ms = clip(ms);
    if !ms.is_finite() {
        return "Invalid Date".into();
    }
    let offset = offset_at(ms);
    let p = parts(ms + offset as f64 * 1000.0).unwrap();
    let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let year = if p.year < 0 {
        format!("-{:04}", -p.year)
    } else {
        format!("{:04}", p.year)
    };
    let zone = offset.unsigned_abs();
    // Chrono exposes the numeric host offset, not Node/ICU's localized long name.
    format!(
        "{} {} {:02} {year} {:02}:{:02}:{:02} GMT{}{:02}{:02}",
        weekdays[p.day as usize],
        months[p.month as usize - 1],
        p.date,
        p.hours,
        p.minutes,
        p.seconds,
        if offset < 0 { '-' } else { '+' },
        zone / 3600,
        zone / 60 % 60
    )
}

fn digits(bytes: &[u8], position: &mut usize, count: usize) -> Option<i64> {
    let slice = bytes.get(*position..position.checked_add(count)?)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = slice
        .iter()
        .fold(0i64, |value, digit| value * 10 + (digit - b'0') as i64);
    *position += count;
    Some(value)
}

fn parse_iso(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let year = match bytes.first() {
        Some(b'+' | b'-') => {
            let negative = bytes[0] == b'-';
            i += 1;
            let value = digits(bytes, &mut i, 6)?;
            if negative && value == 0 {
                return None;
            }
            if negative { -value } else { value }
        }
        _ => digits(bytes, &mut i, 4)?,
    };
    let mut month = 1;
    let mut date = 1;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
        month = digits(bytes, &mut i, 2)?;
        if bytes.get(i) == Some(&b'-') {
            i += 1;
            date = digits(bytes, &mut i, 2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&date) {
        return None;
    }
    let day = days_from_civil(year, month, date) as f64;
    if i == bytes.len() {
        return Some(clip(day * DAY as f64));
    } // Date-only means UTC.
    if !matches!(bytes.get(i), Some(b'T' | b't' | b' ')) {
        return None;
    }
    i += 1;
    let hours = digits(bytes, &mut i, 2)?;
    if bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    let minutes = digits(bytes, &mut i, 2)?;
    let mut seconds = 0;
    let mut millis = 0;
    let mut fraction_nonzero = false;
    if bytes.get(i) == Some(&b':') {
        i += 1;
        seconds = digits(bytes, &mut i, 2)?;
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                let digit = bytes[i] - b'0';
                fraction_nonzero |= digit != 0;
                if i - start < 3 {
                    millis = millis * 10 + digit as i64;
                }
                i += 1;
            }
            if i == start {
                return None;
            }
            for _ in (i - start)..3 {
                millis *= 10;
            }
        }
    }
    if hours > 24
        || minutes > 59
        || seconds > 59
        || (hours == 24 && (minutes != 0 || seconds != 0 || fraction_nonzero))
    {
        return None;
    }
    let wall =
        day * DAY as f64 + (hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis) as f64;
    if i == bytes.len() {
        return Some(local_to_utc(wall));
    } // Offset-less datetime means local.
    let offset = match bytes[i] {
        b'Z' | b'z' => {
            i += 1;
            0
        }
        b'+' | b'-' => {
            let sign = if bytes[i] == b'+' { 1 } else { -1 };
            i += 1;
            let hours = digits(bytes, &mut i, 2)?;
            if bytes.get(i) == Some(&b':') {
                i += 1;
            }
            let minutes = digits(bytes, &mut i, 2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 60 + minutes) * 60_000
        }
        _ => return None,
    };
    (i == bytes.len()).then(|| clip(wall - offset as f64))
}

fn parse_display(text: &str) -> Option<f64> {
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() < 6 {
        return None;
    }
    if fields.len() > 6 && !(fields[6].starts_with('(') && text.ends_with(')')) {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| fields[1].eq_ignore_ascii_case(month))? as i64
        + 1;
    let date = fields[2].parse::<i64>().ok()?;
    let year = fields[3].parse::<i64>().ok()?;
    if !(1..=31).contains(&date) || !(-1_000_000..=1_000_000).contains(&year) {
        return None;
    }
    let clock: Vec<_> = fields[4].split(':').collect();
    if clock.len() != 3 {
        return None;
    }
    let h = clock[0].parse::<i64>().ok()?;
    let m = clock[1].parse::<i64>().ok()?;
    let s = clock[2].parse::<i64>().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..60).contains(&s) {
        return None;
    }
    let zone = fields[5].strip_prefix("GMT")?.as_bytes();
    if zone.len() != 5 || !matches!(zone[0], b'+' | b'-') {
        return None;
    }
    let mut i = 1;
    let oh = digits(zone, &mut i, 2)?;
    let om = digits(zone, &mut i, 2)?;
    if oh > 23 || om > 59 {
        return None;
    }
    let offset = (oh * 60 + om) * 60_000 * if zone[0] == b'+' { 1 } else { -1 };
    Some(clip(
        days_from_civil(year, month, date) as f64 * DAY as f64
            + (h * 3_600_000 + m * 60_000 + s * 1000 - offset) as f64,
    ))
}

fn parse(text: &str) -> f64 {
    if let Some(ms) = parse_iso(text) {
        return ms;
    }
    if let Some(ms) = parse_display(text) {
        return ms;
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(text) {
        if dt.timestamp_subsec_nanos() < 1_000_000_000 {
            return clip(dt.timestamp_millis() as f64);
        }
    }
    f64::NAN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(value: VmValue) -> f64 {
        let VmValue::Number(value) = value else {
            panic!("expected number")
        };
        value.get()
    }

    fn id(value: VmValue) -> HeapId {
        let VmValue::Reference(id) = value else {
            panic!("expected Date")
        };
        id
    }

    fn utc(values: &[f64]) -> f64 {
        let args: Vec<_> = values.iter().copied().map(number).collect();
        value(static_call(&mut Heap::default(), "Date.UTC", &args).unwrap())
    }

    #[test]
    fn full_timeclip_range_and_expanded_iso_years() {
        for (ms, text) in [
            (0.0, "1970-01-01T00:00:00.000Z"),
            (-1.0, "1969-12-31T23:59:59.999Z"),
            (-62_167_219_200_000.0, "0000-01-01T00:00:00.000Z"),
            (LIMIT, "+275760-09-13T00:00:00.000Z"),
            (-LIMIT, "-271821-04-20T00:00:00.000Z"),
            (253_402_300_800_000.0, "+010000-01-01T00:00:00.000Z"),
        ] {
            assert_eq!(iso(ms).unwrap(), text);
            assert_eq!(parse(text), ms);
        }
        assert_eq!(clip(-0.9).to_bits(), 0.0f64.to_bits());
        assert_ne!(display(LIMIT), "Invalid Date");
        assert_ne!(display(-LIMIT), "Invalid Date");
        for ms in [
            f64::NAN,
            f64::INFINITY,
            -f64::INFINITY,
            LIMIT + 1.0,
            -LIMIT - 1.0,
        ] {
            assert!(clip(ms).is_nan());
            assert!(iso(ms).unwrap_err().to_string().starts_with("RangeError:"));
            assert_eq!(display(ms), "Invalid Date");
        }
    }

    #[test]
    fn utc_defaults_year_adjustment_and_component_overflow() {
        assert!(utc(&[]).is_nan());
        for (args, expected) in [
            (vec![2024.0], "2024-01-01T00:00:00.000Z"),
            (vec![0.0, 0.0, 1.0], "1900-01-01T00:00:00.000Z"),
            (vec![99.0, 12.0, 0.0], "1999-12-31T00:00:00.000Z"),
            (vec![100.0, 0.0, 1.0], "0100-01-01T00:00:00.000Z"),
            (vec![2024.0, 1.0, 30.0], "2024-03-01T00:00:00.000Z"),
            (vec![2024.0, -1.0, 1.0], "2023-12-01T00:00:00.000Z"),
            (
                vec![2024.0, 0.0, 0.0, 24.0, -1.0, 60.0, -1.0],
                "2023-12-31T23:59:59.999Z",
            ),
        ] {
            assert_eq!(iso(utc(&args)).unwrap(), expected);
        }
        assert!(utc(&[2024.0, f64::NAN]).is_nan());
        assert!(utc(&[1e9, -12e9, 1.0]).is_nan());
        assert_eq!(utc(&[1e6, 0.0, -365_242_000.0]), -62_124_105_600_000.0);
    }

    #[test]
    fn iso_parser_offsets_fractions_and_rejections() {
        for (text, expected) in [
            ("2020", "2020-01-01T00:00:00.000Z"),
            ("2020-02", "2020-02-01T00:00:00.000Z"),
            ("2020-02-30", "2020-03-01T00:00:00.000Z"),
            ("2020T00:00Z", "2020-01-01T00:00:00.000Z"),
            ("2020-01-01T24:00:00Z", "2020-01-02T00:00:00.000Z"),
            ("2020-01-01T00:00:00.1+08:00", "2019-12-31T16:00:00.100Z"),
            (
                "2020-01-01T00:00:00.123456-0130",
                "2020-01-01T01:30:00.123Z",
            ),
        ] {
            assert_eq!(iso(parse(text)).unwrap(), expected, "{text}");
        }
        for text in [
            "",
            "nonsense",
            "2020-13-01",
            "2020-01-00",
            "2020-01-32",
            "2020-01-01T25:00Z",
            "2020-01-01T24:00:00.0001Z",
            "2020-01-01T00:60Z",
            "2020-01-01T00:00:60Z",
            "2020-01-01T00:00:00+24:00",
            "2020-01-01T00:00:00.Z",
            "2020-01-01T00:00:00Zjunk",
            "-000000-01-01T00:00:00Z",
            " 2020-01-01T00:00:00Z ",
        ] {
            assert!(parse(text).is_nan(), "{text}");
        }
    }

    #[test]
    fn constructors_clone_dates_coerce_primitives_and_preserve_invalidity() {
        let mut heap = Heap::default();
        let original = construct(&mut heap, &[number(1234.9)]).unwrap();
        let copied = construct(&mut heap, &[original.clone()]).unwrap();
        assert_ne!(id(original), id(copied.clone()));
        assert_eq!(heap.date_value(id(copied)), Some(1234.0));
        let null = construct(&mut heap, &[VmValue::Null]).unwrap();
        assert_eq!(heap.date_value(id(null)), Some(0.0));
        let invalid = construct(&mut heap, &[VmValue::Undefined]).unwrap();
        let invalid_id = id(invalid.clone());
        let copied = construct(&mut heap, &[invalid]).unwrap();
        assert!(heap.date_value(id(copied)).unwrap().is_nan());
        assert_eq!(
            method(&heap, invalid_id, "toJSON", &[]).unwrap(),
            VmValue::Null
        );
        assert!(value(method(&heap, invalid_id, "getUTCFullYear", &[]).unwrap()).is_nan());
        assert!(method(&heap, invalid_id, "toISOString", &[]).is_err());
        assert!(method(&heap, invalid_id, "setTime", &[]).is_err());
        assert!(
            method(&Heap::default(), invalid_id, "getTime", &[])
                .unwrap_err()
                .to_string()
                .starts_with("TypeError:")
        );
    }

    #[test]
    fn getters_and_local_construction_use_the_host_timezone() {
        let mut heap = Heap::default();
        let date = id(construct(
            &mut heap,
            &[VmValue::String("2024-02-29T12:34:56.789Z".into())],
        )
        .unwrap());
        for (name, expected) in [
            ("FullYear", 2024.0),
            ("Month", 1.0),
            ("Date", 29.0),
            ("Day", 4.0),
            ("Hours", 12.0),
            ("Minutes", 34.0),
            ("Seconds", 56.0),
            ("Milliseconds", 789.0),
        ] {
            assert_eq!(
                value(method(&heap, date, &format!("getUTC{name}"), &[]).unwrap()),
                expected
            );
        }
        let ms = heap.date_value(date).unwrap();
        let p = parts(ms + offset_at(ms) as f64 * 1000.0).unwrap();
        let args = [
            p.year as f64,
            (p.month - 1) as f64,
            p.date as f64,
            p.hours as f64,
            p.minutes as f64,
            p.seconds as f64,
            p.millis as f64,
        ]
        .map(number);
        let local = id(construct(&mut heap, &args).unwrap());
        assert_eq!(heap.date_value(local), Some(ms));
        assert_eq!(
            value(method(&heap, local, "getMonth", &[]).unwrap()),
            (p.month - 1) as f64
        );
        assert_eq!(parse(&display(ms - 789.0)), ms - 789.0);
        assert_eq!(
            parse("2024-02-29T12:34:56.789"),
            local_to_utc(utc(&[2024.0, 1.0, 29.0, 12.0, 34.0, 56.0, 789.0]))
        );
    }

    #[test]
    fn dst_overlap_chooses_earlier_and_gaps_use_prior_offset() {
        let dt = |y, m, d, h, min| {
            NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, min, 0)
                .unwrap()
        };
        let wall = dt(2024, 3, 10, 2, 30);
        let spring = |time| {
            if time >= dt(2024, 3, 10, 2, 0) && time < dt(2024, 3, 10, 3, 0) {
                LocalResult::None
            } else {
                LocalResult::Single(if time < dt(2024, 3, 10, 2, 0) {
                    -5 * 3600
                } else {
                    -4 * 3600
                })
            }
        };
        assert_eq!(
            resolve_local(wall, spring),
            Some(utc(&[2024.0, 2.0, 10.0, 7.0, 30.0]))
        );
        assert_eq!(
            resolve_local(dt(2024, 11, 3, 1, 30), |_| LocalResult::Ambiguous(
                -5 * 3600,
                -4 * 3600
            )),
            Some(utc(&[2024.0, 10.0, 3.0, 5.0, 30.0]))
        );
        let samoa = |time| {
            if time >= dt(2011, 12, 30, 0, 0) && time < dt(2011, 12, 31, 0, 0) {
                LocalResult::None
            } else {
                LocalResult::Single(if time < dt(2011, 12, 30, 0, 0) {
                    -10 * 3600
                } else {
                    14 * 3600
                })
            }
        };
        assert_eq!(
            resolve_local(dt(2011, 12, 30, 12, 0), samoa),
            Some(utc(&[2011.0, 11.0, 30.0, 22.0]))
        );
    }

    #[test]
    fn now_and_both_plain_call_names_ignore_arguments() {
        let mut heap = Heap::default();
        let start = Utc::now().timestamp_millis() as f64;
        let date = id(construct(&mut heap, &[]).unwrap());
        let now = value(static_call(&mut heap, "Date.now", &[]).unwrap());
        let end = Utc::now().timestamp_millis() as f64;
        assert!((start..=end).contains(&heap.date_value(date).unwrap()));
        assert!((start..=end).contains(&now));
        for name in ["Date", "Date.call", "call"] {
            let VmValue::String(text) =
                static_call(&mut heap, name, &[VmValue::Undefined]).unwrap()
            else {
                panic!("string expected")
            };
            assert_ne!(text, "Invalid Date");
            assert!(parse(&text).is_finite());
        }
    }
}
