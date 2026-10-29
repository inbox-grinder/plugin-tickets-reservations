//! Conservative labelled flight confirmations. Event tickets are a separate release slice.
use chrono::{DateTime, NaiveDate, SecondsFormat};
use plugin_sdk::{
    EmailInput, FieldCandidate, RecordContribution, RecordIdentity, RecordResult, RefreshInput,
    RefreshRequest, SemanticUpdate, SourceRef,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
plugin_sdk::export_plugin!(extract);
plugin_sdk::export_refresh!(refresh_report);

const OPERATIONAL: [&str; 6] = [
    "flightNumber",
    "origin",
    "destination",
    "departureAt",
    "arrivalAt",
    "status",
];
fn empty(sources: Vec<SourceRef>) -> RecordResult {
    RecordResult {
        schema_version: "1".into(),
        sources,
        records: vec![],
        refresh_requests: vec![],
    }
}
fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn timestamp(s: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M%:z"))
        .ok()
        .map(|d| d.to_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
}
fn label<'a>(lines: &'a [String], name: &str) -> Option<&'a str> {
    let values: BTreeSet<_> = lines
        .iter()
        .filter_map(|s| s.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
        .collect();
    (values.len() == 1)
        .then(|| *values.first().unwrap())
        .filter(|s| !s.is_empty())
}
fn code(s: &str, min: usize, max: usize) -> Option<String> {
    let s = s.trim().to_ascii_uppercase();
    ((min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(s)
}
fn day(s: &str) -> Option<String> {
    (s.len() == 10)
        .then(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .flatten()
        .map(|d| d.to_string())
}
fn flight(s: &str) -> Option<String> {
    let s = s
        .split_whitespace()
        .collect::<String>()
        .to_ascii_uppercase();
    let (carrier, number) = s.split_at_checked(2)?;
    (carrier.bytes().all(|b| b.is_ascii_alphanumeric())
        && carrier.bytes().any(|b| b.is_ascii_alphabetic())
        && (1..=4).contains(&number.len())
        && number.bytes().all(|b| b.is_ascii_digit()))
    .then(|| format!("{carrier}{}", number.parse::<u16>().unwrap()))
}
fn status(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "confirmed" | "scheduled" | "on time" => Some("scheduled"),
        "delayed" => Some("delayed"),
        "cancelled" | "canceled" => Some("cancelled"),
        "landed" | "arrived" => Some("landed"),
        _ => None,
    }
}
fn path(number: &str, date: &str) -> String {
    format!(
        "/v2/schedule/flights?carrierCode={}&flightNumber={}&scheduledDepartureDate={date}",
        &number[..2],
        &number[2..]
    )
}
fn contribution(
    id: RecordIdentity,
    values: BTreeMap<String, Value>,
    source: &str,
    observed: &str,
    records: &[Value],
) -> RecordContribution {
    let facts: BTreeMap<_, _> = OPERATIONAL
        .iter()
        .map(|k| (*k, values[*k].clone()))
        .collect();
    let update_key = format!("flight:{}", hash(&json!(facts)));
    let occurred = records
        .iter()
        .find(|r| r["identity"] == json!(id))
        .and_then(|r| r["updates"][&update_key]["occurredAt"].as_str())
        .unwrap_or(observed)
        .to_owned();
    let title = format!(
        "{} · {} → {}",
        values["flightNumber"].as_str().unwrap(),
        values["origin"].as_str().unwrap(),
        values["destination"].as_str().unwrap()
    );
    let summary = format!(
        "{} · {} · {}",
        title,
        values["status"].as_str().unwrap(),
        values["departureAt"].as_str().unwrap()
    );
    let completed = matches!(values["status"].as_str(), Some("landed" | "cancelled"));
    RecordContribution {
        identity: id,
        record_type: "flight".into(),
        fields: values
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    FieldCandidate {
                        value,
                        evidence: vec![source.into()],
                        basis: "extracted".into(),
                        confidence: 1.0,
                        observed_at: occurred.clone(),
                        revision: None,
                    },
                )
            })
            .collect(),
        updates: vec![SemanticUpdate {
            key: update_key,
            occurred_at: occurred.clone(),
            summary,
            notify: true,
            evidence: vec![source.into()],
            fields: OPERATIONAL.iter().map(|s| s.to_string()).collect(),
            supersedes: None,
        }],
        attention: vec![],
        links: vec![],
        presentation: json!({"family":"flight","version":1,"title":title}),
        lifecycle: if completed {
            json!({"state":"completed","completedAt":occurred})
        } else {
            json!({"state":"active"})
        },
    }
}

pub fn extract(input: EmailInput) -> RecordResult {
    let mut result = empty(
        input
            .invocation
            .as_ref()
            .map(|i| i.sources.clone())
            .unwrap_or_default(),
    );
    let Some(source) = input.message.source_id.as_ref().filter(|id| {
        result
            .sources
            .iter()
            .any(|s| &s.id == *id && s.kind == "email")
    }) else {
        return result;
    };
    let Some(observed) = input
        .date
        .as_deref()
        .and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .or_else(|| DateTime::parse_from_rfc2822(s).ok())
        })
        .map(|d| d.to_utc().to_rfc3339_opts(SecondsFormat::Secs, true))
    else {
        return result;
    };
    let text = input
        .text_body
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| html_text(input.html_body.as_deref().unwrap_or("")));
    let lines: Vec<String> = text
        .lines()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect();
    let start = lines
        .iter()
        .position(|s| s.to_ascii_lowercase().starts_with("segment:"));
    let Some(start) = start else { return result };
    let header = &lines[..start];
    let Some(airline) = label(header, "airline").and_then(|s| code(s, 2, 2)) else {
        return result;
    };
    let Some(confirmation) = label(header, "confirmation").and_then(|s| code(s, 5, 12)) else {
        return result;
    };
    let Some(booked) = label(header, "booking date").and_then(day) else {
        return result;
    };
    let passengers: BTreeSet<_> = header
        .iter()
        .filter_map(|s| s.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case("passenger"))
        .map(|(_, v)| v.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if passengers.is_empty() {
        return result;
    }
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .skip(start)
        .filter(|(_, s)| s.to_ascii_lowercase().starts_with("segment:"))
        .map(|(i, _)| i)
        .chain(std::iter::once(lines.len()))
        .collect();
    let mut seen = BTreeSet::new();
    for range in starts.windows(2) {
        let segment = &lines[range[0]..range[1]];
        let Some(reference) = label(segment, "segment").and_then(|s| code(s, 1, 32)) else {
            return empty(result.sources);
        };
        if !seen.insert(reference.clone()) {
            return empty(result.sources);
        }
        let (
            Some(number),
            Some(origin),
            Some(destination),
            Some(scheduled),
            Some(departure),
            Some(arrival),
            Some(state),
        ) = (
            label(segment, "flight").and_then(flight),
            label(segment, "origin").and_then(|s| code(s, 3, 3)),
            label(segment, "destination").and_then(|s| code(s, 3, 3)),
            label(segment, "scheduled departure"),
            label(segment, "departure"),
            label(segment, "arrival"),
            label(segment, "status").and_then(status),
        )
        else {
            continue;
        };
        let (Some(scheduled_utc), Some(dep), Some(arr)) = (
            timestamp(scheduled),
            timestamp(departure),
            timestamp(arrival),
        ) else {
            continue;
        };
        if dep >= arr || origin == destination || booked.as_str() > &scheduled[..10] {
            continue;
        }
        let identity = RecordIdentity {
            namespace: format!("flight:{airline}"),
            key: hash(&json!([confirmation, booked, reference])),
        };
        let values = BTreeMap::from([
            ("airline".into(), json!(airline)),
            ("confirmation".into(), json!(confirmation)),
            ("bookingDate".into(), json!(booked)),
            ("segment".into(), json!(reference)),
            ("passengers".into(), json!(passengers)),
            ("flightNumber".into(), json!(number)),
            ("origin".into(), json!(origin)),
            ("destination".into(), json!(destination)),
            ("status".into(), json!(state)),
            ("scheduledDepartureAt".into(), json!(scheduled_utc)),
            ("scheduledDepartureDate".into(), json!(&scheduled[..10])),
            ("departureAt".into(), json!(dep)),
            ("arrivalAt".into(), json!(arr)),
            ("departureLocal".into(), json!(departure)),
            ("arrivalLocal".into(), json!(arrival)),
        ]);
        let records = input
            .invocation
            .as_ref()
            .map(|i| i.records.as_slice())
            .unwrap_or_default();
        // A request is a proposal; the host alone grants and dispatches it.
        if !matches!(state, "landed" | "cancelled") {
            result.refresh_requests.push(RefreshRequest {
                identity: identity.clone(),
                service: "amadeus".into(),
                path: path(&number, &scheduled[..10]),
                interval_seconds: 1800,
            });
        }
        result
            .records
            .push(contribution(identity, values, source, &observed, records));
    }
    result.refresh_requests.truncate(16);
    result
}

pub fn refresh_report(input: RefreshInput) -> RecordResult {
    let mut result = empty(input.invocation.sources.clone());
    if input.schema_version != "1"
        || input.invocation.kind != "refresh"
        || input.service.id != "amadeus"
        || !result
            .sources
            .iter()
            .any(|s| s.id == input.service.source_id && s.kind == "remote")
    {
        return result;
    }
    let Some(observed) = input.service.fetched_at.as_deref().and_then(timestamp) else {
        return result;
    };
    // The host dispatches one leased record per job. Never guess among bookings or flights.
    if input.invocation.records.len() != 1 {
        return result;
    }
    let record = &input.invocation.records[0];
    let value = |k: &str| record["fields"][k]["candidate"]["value"].as_str();
    let (Some(number), Some(date), Some(origin), Some(destination), Some(scheduled)) = (
        value("flightNumber"),
        value("scheduledDepartureDate"),
        value("origin"),
        value("destination"),
        value("scheduledDepartureAt"),
    ) else {
        return result;
    };
    if flight(number).as_deref() != Some(number)
        || day(date).is_none()
        || input.service.resource != format!("https://api.amadeus.com{}", path(number, date))
    {
        return result;
    }
    let Some(data) = input.service.response["data"].as_array() else {
        return result;
    };
    let matching: Vec<_> = data
        .iter()
        .filter(|f| {
            f["scheduledDepartureDate"] == date
                && f["flightDesignator"]["carrierCode"] == &number[..2]
                && f["flightDesignator"]["flightNumber"].as_u64() == number[2..].parse::<u64>().ok()
        })
        .collect();
    if matching.len() != 1 {
        return result;
    }
    let Some(points) = matching[0]["flightPoints"].as_array() else {
        return result;
    };
    let endpoint = |airport: &str, direction: &str| -> Option<&Value> {
        let ps: Vec<_> = points
            .iter()
            .filter(|p| p["iataCode"] == airport && p[direction].is_object())
            .collect();
        (ps.len() == 1).then(|| &ps[0][direction])
    };
    let (Some(departure), Some(arrival)) = (
        endpoint(origin, "departure"),
        endpoint(destination, "arrival"),
    ) else {
        return result;
    };
    let timing = |point: &Value, qualifiers: &[&str]| -> Option<String> {
        let timings = point["timings"].as_array()?;
        for q in qualifiers {
            let ts: Vec<_> = timings.iter().filter(|t| t["qualifier"] == *q).collect();
            if ts.len() > 1 {
                return None;
            }
            if let Some(t) = ts.first() {
                let raw = t["value"].as_str()?;
                timestamp(raw)?;
                return Some(raw.to_owned());
            }
        }
        None
    };
    if timing(departure, &["STD"])
        .as_deref()
        .and_then(timestamp)
        .as_deref()
        != Some(scheduled)
    {
        return result;
    }
    let (Some(dep), Some(arr)) = (
        timing(departure, &["ATD", "ETD", "STD"]),
        timing(arrival, &["ATA", "ETA", "STA"]),
    ) else {
        return result;
    };
    let dep_utc = timestamp(&dep).unwrap();
    let arr_utc = timestamp(&arr).unwrap();
    if dep_utc >= arr_utc {
        return result;
    }
    let state = if timing(arrival, &["ATA"]).is_some() {
        "landed"
    } else if dep_utc.as_str() > scheduled {
        "delayed"
    } else {
        "scheduled"
    };
    let values = BTreeMap::from([
        ("flightNumber".into(), json!(number)),
        ("origin".into(), json!(origin)),
        ("destination".into(), json!(destination)),
        ("departureAt".into(), json!(dep_utc)),
        ("arrivalAt".into(), json!(arr_utc)),
        ("departureLocal".into(), json!(dep)),
        ("arrivalLocal".into(), json!(arr)),
        ("status".into(), json!(state)),
    ]);
    let Ok(identity) = serde_json::from_value::<RecordIdentity>(record["identity"].clone()) else {
        return result;
    };
    if !identity.namespace.starts_with("flight:")
        || matches!(value("status"), Some("landed" | "cancelled"))
    {
        return result;
    }
    result.records.push(contribution(
        identity,
        values,
        &input.service.source_id,
        &observed,
        &input.invocation.records,
    ));
    result
}

fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut inside = false;
    let mut hidden = false;
    for c in html.chars() {
        match c {
            '<' => {
                inside = true;
                tag.clear();
            }
            '>' if inside => {
                inside = false;
                let name = tag
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ["script", "style"].contains(&name.as_str()) {
                    hidden = true
                }
                if ["/script", "/style"].contains(&name.as_str()) {
                    hidden = false
                }
                if !hidden
                    && ["br", "br/", "/p", "/div", "/tr", "/li", "/h1", "/h2"]
                        .contains(&name.as_str())
                {
                    out.push('\n')
                }
                if !hidden && ["/td", "/th"].contains(&name.as_str()) {
                    out.push(' ')
                }
            }
            _ if inside => tag.push(c),
            _ if !hidden => out.push(c),
            _ => {}
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> EmailInput {
        serde_json::from_str(include_str!("../fixtures/confirmation.json")).unwrap()
    }
    #[test]
    fn itinerary_identity_survives_changes_but_not_reused_bookings() {
        let initial = extract(input());
        let changed = extract(
            serde_json::from_str(include_str!("../fixtures/changed-itinerary.json")).unwrap(),
        );
        assert_eq!(initial.records[0].identity, changed.records[0].identity);
        assert_ne!(
            initial.records[0].updates[0].key,
            changed.records[0].updates[0].key
        );
        let itinerary =
            extract(serde_json::from_str(include_str!("../fixtures/itinerary.json")).unwrap());
        assert_eq!(itinerary.records.len(), 2);
        assert_ne!(itinerary.records[0].identity, itinerary.records[1].identity);
        assert_eq!(
            itinerary.records[0].fields["passengers"].value,
            json!(["Alex Example", "Sam Example"])
        );
        let mut other = input();
        other.text_body = other
            .text_body
            .map(|s| s.replace("Booking date: 2026-10-24", "Booking date: 2026-10-25"));
        assert_ne!(
            initial.records[0].identity,
            extract(other).records[0].identity
        );
    }
    #[test]
    fn transport_and_offset_format_do_not_change_semantic_identity() {
        let initial = extract(input());
        let mut html = input();
        html.html_body = html
            .text_body
            .take()
            .map(|s| format!("<div>{}</div>", s.replace('\n', "</div><div>")));
        assert_eq!(
            serde_json::to_value(&initial).unwrap(),
            serde_json::to_value(extract(html)).unwrap()
        );
        let mut utc = input();
        utc.text_body = utc.text_body.map(|s| {
            s.replace(
                "Departure: 2026-10-26T11:40:00+01:00",
                "Departure: 2026-10-26T10:40:00Z",
            )
            .replace(
                "Arrival: 2026-10-26T13:00:00+01:00",
                "Arrival: 2026-10-26T12:00:00Z",
            )
        });
        assert_eq!(
            initial.records[0].updates[0].key,
            extract(utc).records[0].updates[0].key
        );
        assert_eq!(
            initial.records[0].fields["departureAt"].value,
            "2026-10-26T10:40:00Z"
        );
        assert_eq!(
            initial.records[0].fields["departureLocal"].value,
            "2026-10-26T11:40:00+01:00"
        );
    }
    #[test]
    fn remote_matches_email_delay_and_rejects_mismatched_flight_evidence() {
        let initial = extract(input());
        let record = &initial.records[0];
        let fields: BTreeMap<_, _> = record
            .fields
            .iter()
            .map(|(k, v)| (k, json!({"candidate":v})))
            .collect();
        let context = json!({"identity":record.identity,"fields":fields,"updates":{}});
        let response: Value =
            serde_json::from_str(include_str!("../fixtures/amadeus-delay.json")).unwrap();
        let mut remote = json!({"schemaVersion":"1","invocation":{"schemaVersion":"1","kind":"refresh","capabilities":["records","remote-service","scheduled-refresh"],"config":{},"records":[context],"sources":[{"id":"remote","kind":"remote"}]},"service":{"id":"amadeus","resource":format!("https://api.amadeus.com{}",initial.refresh_requests[0].path),"sourceId":"remote","fetchedAt":"2026-10-26T09:00:00Z","response":response}});
        let run = |v: Value| refresh_report(serde_json::from_value(v).unwrap());
        let delayed = run(remote.clone());
        let email = extract(serde_json::from_str(include_str!("../fixtures/delay.json")).unwrap());
        assert_eq!(
            delayed.records[0].updates[0].key,
            email.records[0].updates[0].key
        );
        assert_eq!(
            delayed.records[0].updates[0].summary,
            email.records[0].updates[0].summary
        );
        assert!(
            !delayed.records[0].fields.contains_key("passengers"),
            "remote must not supply evidence for passenger facts"
        );
        let mut missing = remote.clone();
        missing["service"]
            .as_object_mut()
            .unwrap()
            .remove("fetchedAt");
        assert!(run(missing).records.is_empty());
        for (field, value) in [
            ("scheduledDepartureDate", json!("2026-10-27")),
            (
                "flightDesignator",
                json!({"carrierCode":"IB","flightNumber":533}),
            ),
        ] {
            let mut wrong = remote.clone();
            wrong["service"]["response"]["data"][0][field] = value;
            assert!(run(wrong).records.is_empty());
        }
        let mut ambiguous = remote.clone();
        ambiguous["service"]["response"]["data"] =
            json!([response["data"][0], response["data"][0]]);
        assert!(run(ambiguous).records.is_empty());
        // Provider minute-precision times still identify the same event.
        remote["service"]["response"]["data"][0]["flightPoints"][0]["departure"]["timings"][1]
            ["value"] = json!("2026-10-26T12:10+01:00");
        assert_eq!(
            run(remote.clone()).records[0].updates[0].key,
            email.records[0].updates[0].key
        );
        remote["service"]["response"]["data"][0]["flightPoints"][1]["arrival"]["timings"][1]
            ["qualifier"] = json!("ATA");
        assert_eq!(run(remote).records[0].lifecycle["state"], "completed");
    }
    #[test]
    fn incomplete_or_ambiguous_confirmations_fall_back_to_mail() {
        for (old, new) in [
            ("Passenger:", "Recipient:"),
            ("Booking date:", "Date:"),
            ("+01:00", ""),
            ("Flight: IB532", "Flight: IB532\nFlight: IB534"),
            ("Origin: MAD", "Origin: MAD\nOrigin: LIS"),
        ] {
            let mut i = input();
            i.text_body = i.text_body.map(|s| s.replace(old, new));
            assert!(extract(i).records.is_empty(), "{old}");
        }
        let mut no_source = input();
        no_source.message.source_id = None;
        assert!(extract(no_source).records.is_empty());
        let mut offline = input();
        offline.invocation.as_mut().unwrap().capabilities = vec!["records".into()];
        let parsed = extract(offline);
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.refresh_requests.len(), 1);
        // Event-ticket mail remains ordinary mail until its separate parser is implemented.
        let mut event = input();
        event.text_body = Some("Event: Concert\nTicket: T123\nDate: 2026-10-26".into());
        assert!(extract(event).records.is_empty());
    }
}
