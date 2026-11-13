//! Tests of the EWS calendar backend over the sanitised answers of the spike
//! (`tests/fixtures/ews/*.xml`, `.agents/research/2026-11-13-ews-spike.md`,
//! "Fixtures"): names, addresses, subjects, ids and sync states are
//! placeholders, element names, attributes and value formats are the
//! server's.

const CALENDAR_VIEW: &str = include_str!("../../../tests/fixtures/ews/finditem_calendarview.xml");

/// Every `Id` attribute of a `StartTimeZone` or `EndTimeZone` element in `xml`.
fn zone_ids(xml: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    for tag in ["<t:StartTimeZone ", "<t:EndTimeZone "] {
        for (at, _) in xml.match_indices(tag) {
            let element = &xml[at..at + xml[at..].find('>').expect("the tag closes")];
            let id = element
                .split(" Id=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("the zone carries an Id");
            ids.push(id);
        }
    }
    ids
}

/// The CalendarView fixture names its zones by the Windows ids the server
/// sent (`W. Europe Standard Time`), which the Windows-to-IANA table maps,
/// and its periods by the server's registry rules; the sanitiser once
/// scrambled both.
#[test]
fn the_calendar_view_fixture_names_windows_zones() {
    let ids = zone_ids(CALENDAR_VIEW);
    assert_eq!(
        ids.len(),
        6,
        "a start and an end zone on each of three items"
    );
    for id in ids {
        assert_eq!(
            crate::windows_zones::windows_zone_to_iana(id),
            Some("Europe/Berlin"),
            "{id:?}"
        );
    }
    for rule in ["1-Standard", "1-Daylight"] {
        let id = format!("Id=\"trule:Microsoft/Registry/W. Europe Standard Time/{rule}\"");
        assert_eq!(CALENDAR_VIEW.matches(&id).count(), 6, "{rule}");
    }
}
