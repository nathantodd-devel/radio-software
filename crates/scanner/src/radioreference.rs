//! Importing systems straight from RadioReference.com through its web
//! service (a SOAP API), for accounts that have access to it: a premium
//! subscription and an application key issued by RadioReference.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use roxmltree::{Document, Node};

use crate::plan::{Kind, P25Frequency, Plan, conventional_kind, count_note, entry};

const ENDPOINT: &str = "https://api.radioreference.com/soap2/index.php";
const NAMESPACE: &str = "http://api.radioreference.com/soap2";
const TIMEOUT: Duration = Duration::from_secs(30);

/// A RadioReference login. The password is the account's; the application
/// key is one RadioReference issues to developers on request.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub app_key: String,
}

/// A system ready to be added to the channel database.
pub struct Imported {
    pub name: String,
    pub location: String,
    pub plan: Plan,
    /// What was left out, and anything else worth knowing.
    pub notes: Vec<String>,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The request for `method` with one integer parameter.
fn envelope(method: &str, param: &str, value: i64, creds: &Credentials) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/" xmlns:ns1="{NAMESPACE}" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" SOAP-ENV:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><SOAP-ENV:Body><ns1:{method}><{param} xsi:type="xsd:int">{value}</{param}>{extra}<authInfo xsi:type="ns1:authInfo"><username xsi:type="xsd:string">{username}</username><password xsi:type="xsd:string">{password}</password><appKey xsi:type="xsd:string">{app_key}</appKey><version xsi:type="xsd:string">latest</version><style xsi:type="xsd:string">rpc</style></authInfo></ns1:{method}></SOAP-ENV:Body></SOAP-ENV:Envelope>"#,
        // getTrsTalkgroups also takes filters; zero means "all".
        extra = if method == "getTrsTalkgroups" {
            r#"<tgCid xsi:type="xsd:int">0</tgCid><tgTag xsi:type="xsd:int">0</tgTag><tgDec xsi:type="xsd:int">0</tgDec>"#
        } else {
            ""
        },
        username = escape(&creds.username),
        password = escape(&creds.password),
        app_key = escape(&creds.app_key),
    )
}

/// Call `method` and return the response document's text.
fn call(method: &str, param: &str, value: i64, creds: &Credentials) -> Result<String, String> {
    let sent = ureq::post(ENDPOINT)
        .timeout(TIMEOUT)
        .set("Content-Type", "text/xml; charset=utf-8")
        .set("SOAPAction", &format!("\"{NAMESPACE}#{method}\""))
        .send_string(&envelope(method, param, value, creds));
    let response = match sent {
        // Errors such as a bad login come back as a fault document with an
        // error status; `returned` turns those into messages.
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Err(format!("can't reach RadioReference: {e}")),
    };
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(64 << 20)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("RadioReference: {e}"))?;
    // The service answers in ISO-8859-1, whose bytes are the code points.
    Ok(String::from_utf8(bytes).unwrap_or_else(|e| e.into_bytes().iter().map(|&b| b as char).collect()))
}

/// A child element's text, by name.
fn text<'a>(node: Node<'a, '_>, name: &str) -> &'a str {
    child(node, name).and_then(|n| n.text()).unwrap_or("").trim()
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.has_tag_name(name))
}

/// The elements of an array-valued child.
fn items<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Vec<Node<'a, 'i>> {
    child(node, name).map_or(Vec::new(), |list| list.children().filter(Node::is_element).collect())
}

/// Parse a response and hand its `return` element to `read`; a fault
/// becomes its message.
fn returned<T>(xml: &str, read: impl FnOnce(Node) -> Result<T, String>) -> Result<T, String> {
    // The declared encoding no longer applies once the text is a string.
    let xml = xml
        .trim_start()
        .strip_prefix("<?xml")
        .and_then(|rest| rest.split_once("?>"))
        .map_or(xml, |(_, body)| body);
    let doc = Document::parse(xml).map_err(|e| format!("RadioReference sent something unreadable: {e}"))?;
    if let Some(fault) = doc.descendants().find(|n| n.has_tag_name("faultstring")) {
        return Err(format!(
            "RadioReference: {}",
            fault.text().unwrap_or("request refused").trim()
        ));
    }
    let node = doc
        .descendants()
        .find(|n| n.has_tag_name("return"))
        .ok_or("RadioReference sent a reply with nothing in it")?;
    read(node)
}

/// Talkgroups of a trunked system, as a plan without frequencies.
fn talkgroups(xml: &str) -> Result<(Plan, Vec<String>), String> {
    returned(xml, |list| {
        let (mut plan, mut encrypted, mut other_modes) = (Plan::default(), 0, 0);
        for tg in list.children().filter(Node::is_element) {
            // D is digital; an E after it, or enc of 2, means always
            // encrypted. Anything else is analog or P25 Phase 2.
            let mode = text(tg, "tgMode");
            if mode == "DE" || text(tg, "enc") == "2" {
                encrypted += 1;
            } else if !matches!(mode, "D" | "De") {
                other_modes += 1;
            } else if let Ok(id) = text(tg, "tgDec").parse() {
                plan.entries.push(entry(
                    Kind::Talkgroup { system: 0, id },
                    text(tg, "tgAlpha"),
                    text(tg, "tgDescr"),
                ));
            }
        }
        let mut notes = Vec::new();
        count_note(&mut notes, encrypted, "talkgroups left out: encrypted");
        count_note(&mut notes, other_modes, "talkgroups left out: not P25 Phase 1 voice");
        Ok((plan, notes))
    })
}

/// Every frequency of every site of a trunked system, in Hz.
fn site_frequencies(xml: &str) -> Result<Vec<f64>, String> {
    returned(xml, |sites| {
        let mut freqs: Vec<f64> = Vec::new();
        for site in sites.children().filter(Node::is_element) {
            for freq in items(site, "siteFreqs") {
                if let Ok(mhz) = text(freq, "freq").parse::<f64>() {
                    let hz = (mhz * 1e6).round();
                    if hz > 0.0 && !freqs.contains(&hz) {
                        freqs.push(hz);
                    }
                }
            }
        }
        Ok(freqs)
    })
}

/// A county's name, and its categories as (name, subcategory ids).
#[allow(clippy::type_complexity)]
fn county(xml: &str) -> Result<(String, Vec<(String, Vec<i64>)>), String> {
    returned(xml, |info| {
        let cats = items(info, "cats")
            .into_iter()
            .map(|cat| {
                let subcats = items(cat, "subcats")
                    .into_iter()
                    .filter_map(|sub| text(sub, "scid").parse().ok());
                (text(cat, "cName").to_string(), subcats.collect())
            })
            .collect();
        Ok((text(info, "countyName").to_string(), cats))
    })
}

/// Mode numbers and their names ("FM", "P25"...).
fn modes(xml: &str) -> Result<HashMap<String, String>, String> {
    returned(xml, |list| {
        let pairs = list.children().filter(Node::is_element);
        Ok(pairs
            .map(|m| (text(m, "mode").to_string(), text(m, "modeName").to_string()))
            .collect())
    })
}

/// Add a subcategory's frequencies to `plan`, counting what can't be used
/// in `left_out` and channels with digital squelch codes in `coded`.
fn frequencies(
    xml: &str,
    modes: &HashMap<String, String>,
    plan: &mut Plan,
    left_out: &mut usize,
    coded: &mut usize,
) -> Result<(), String> {
    returned(xml, |list| {
        for freq in list.children().filter(Node::is_element) {
            // The mode is given by number; fall back to taking it as a name.
            let mode = text(freq, "mode");
            let mode = modes.get(mode).map_or(mode, String::as_str);
            let freq_hz = text(freq, "out").parse::<f64>().unwrap_or(0.0) * 1e6;
            let kind = conventional_kind(mode, text(freq, "tone"), freq_hz.round());
            match kind.filter(|_| freq_hz > 0.0 && text(freq, "enc") != "2") {
                Some((kind, squelch_coded)) => {
                    let (tag, desc) = (text(freq, "alpha"), text(freq, "descr"));
                    plan.entries
                        .push(entry(kind, if tag.is_empty() { desc } else { tag }, desc));
                    *coded += squelch_coded as usize;
                }
                None => *left_out += 1,
            }
        }
        Ok(())
    })
}

/// Fetch a trunked system by its RadioReference system ID (the number in
/// its page address, radioreference.com/db/sid/…).
pub fn trunked_system(creds: &Credentials, sid: i64) -> Result<Imported, String> {
    let (name, location) = returned(&call("getTrsDetails", "sid", sid, creds)?, |trs| {
        Ok((text(trs, "sName").to_string(), text(trs, "sCity").to_string()))
    })?;
    let (mut plan, notes) = talkgroups(&call("getTrsTalkgroups", "sid", sid, creds)?)?;
    let sites = site_frequencies(&call("getTrsSites", "sid", sid, creds)?)?;
    if plan.entries.is_empty() {
        return Err(format!(
            "{name}: no talkgroups this scanner can receive ({})",
            if notes.is_empty() {
                "the system lists none".to_string()
            } else {
                notes.join("; ")
            }
        ));
    }
    if sites.is_empty() {
        return Err(format!("{name}: RadioReference lists no site frequencies for it"));
    }
    plan.p25 = sites
        .into_iter()
        .map(|freq_hz| P25Frequency { system: 0, freq_hz })
        .collect();
    plan.add_other_talkgroups();
    Ok(Imported {
        name,
        location,
        plan,
        notes,
    })
}

/// Fetch a county's conventional frequencies by its RadioReference county
/// ID (radioreference.com/db/browse/ctid/…), one system per category.
pub fn county_systems(creds: &Credentials, ctid: i64) -> Result<Vec<Imported>, String> {
    let (county_name, cats) = county(&call("getCountyInfo", "ctid", ctid, creds)?)?;
    let modes = modes(&call("getMode", "mode", 0, creds)?)?;
    let mut systems = Vec::new();
    for (name, subcats) in cats {
        let (mut plan, mut left_out, mut coded) = (Plan::default(), 0, 0);
        for scid in subcats {
            frequencies(
                &call("getSubcatFreqs", "scid", scid, creds)?,
                &modes,
                &mut plan,
                &mut left_out,
                &mut coded,
            )?;
        }
        if plan.entries.is_empty() {
            continue;
        }
        let mut notes = Vec::new();
        count_note(
            &mut notes,
            left_out,
            "channels left out: not analog FM or unencrypted P25",
        );
        count_note(
            &mut notes,
            coded,
            "channels use a digital squelch code; they will open on any signal",
        );
        systems.push(Imported {
            name,
            location: county_name.clone(),
            plan,
            notes,
        });
    }
    if systems.is_empty() {
        return Err(format!(
            "{county_name}: no conventional channels this scanner can receive"
        ));
    }
    Ok(systems)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply shaped as the service's definition describes.
    fn reply(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="ISO-8859-1"?><SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:ns1="{NAMESPACE}"><SOAP-ENV:Body><ns1:response><return>{body}</return></ns1:response></SOAP-ENV:Body></SOAP-ENV:Envelope>"#
        )
    }

    #[test]
    fn fault_becomes_its_message() {
        // What the service really answers to a bad login.
        let fault = r#"<?xml version="1.0" encoding="ISO-8859-1"?><SOAP-ENV:Envelope SOAP-ENV:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"
  xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/"
  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<SOAP-ENV:Body><SOAP-ENV:Fault><faultcode xsi:type="xsd:string" xmlns:xsd="x">AUTH</faultcode><faultstring>Invalid Username or Password.</faultstring></SOAP-ENV:Fault></SOAP-ENV:Body></SOAP-ENV:Envelope>"#;
        assert_eq!(
            talkgroups(fault).err().unwrap(),
            "RadioReference: Invalid Username or Password."
        );
    }

    #[test]
    fn reads_talkgroups_and_sites() {
        let (plan, notes) = talkgroups(&reply(
            "<item><tgDec>865</tgDec><tgAlpha>SO Pat 1</tgAlpha><tgDescr>Patrol &amp; Primary</tgDescr><tgMode>D</tgMode><enc>0</enc></item>\
             <item><tgDec>849</tgDec><tgAlpha>NTF 1</tgAlpha><tgDescr>Narcotics</tgDescr><tgMode>DE</tgMode><enc>2</enc></item>\
             <item><tgDec>100</tgDec><tgAlpha>Old</tgAlpha><tgDescr>Analog</tgDescr><tgMode>A</tgMode><enc>0</enc></item>",
        ))
        .unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert!(matches!(plan.entries[0].kind, Kind::Talkgroup { id: 865, .. }));
        assert_eq!(plan.entries[0].desc, "Patrol & Primary");
        assert_eq!(notes.len(), 2);

        let sites = site_frequencies(&reply(
            "<item><siteDescr>Simulcast</siteDescr><siteFreqs><item><freq>772.03125</freq><use>a</use></item><item><freq>770.03125</freq><use></use></item></siteFreqs></item>\
             <item><siteDescr>Brisbane</siteDescr><siteFreqs><item><freq>772.03125</freq><use>a</use></item><item><freq>771.05625</freq><use></use></item></siteFreqs></item>",
        ))
        .unwrap();
        assert_eq!(sites, [772_031_250.0, 770_031_250.0, 771_056_250.0]);
    }

    #[test]
    fn reads_county_frequencies() {
        let (name, cats) = county(&reply(
            "<countyName>San Mateo</countyName><cats><item><cName>Law</cName><subcats><item><scid>11</scid><scName>PD</scName></item><item><scid>12</scid></item></subcats></item></cats>",
        ))
        .unwrap();
        assert_eq!(
            (name.as_str(), cats),
            ("San Mateo", vec![("Law".to_string(), vec![11, 12])])
        );

        let modes = modes(&reply(
            "<item><mode>1</mode><modeName>FM</modeName></item><item><mode>2</mode><modeName>P25</modeName></item><item><mode>3</mode><modeName>DMR</modeName></item>",
        ))
        .unwrap();
        let (mut plan, mut left_out, mut coded) = (Plan::default(), 0, 0);
        frequencies(
            &reply(
                "<item><out>488.3125</out><descr>Dispatch</descr><alpha>PD 1</alpha><tone>114.8 PL</tone><mode>1</mode><enc>0</enc></item>\
                 <item><out>460.025</out><descr>Car to car</descr><alpha></alpha><tone>023 DPL</tone><mode>FM</mode><enc>0</enc></item>\
                 <item><out>453.1</out><descr>Digital</descr><alpha>PD 3</alpha><tone>293 NAC</tone><mode>2</mode><enc>0</enc></item>\
                 <item><out>482.8875</out><descr>Secure</descr><alpha>PD 4</alpha><tone>9EE NAC</tone><mode>2</mode><enc>2</enc></item>\
                 <item><out>451.2</out><descr>Works</descr><alpha>PW</alpha><tone></tone><mode>3</mode><enc>0</enc></item>",
            ),
            &modes,
            &mut plan,
            &mut left_out,
            &mut coded,
        )
        .unwrap();
        assert_eq!((plan.entries.len(), left_out, coded), (3, 2, 1));
        assert!(matches!(plan.entries[0].kind, Kind::Analog { tone_hz: Some(t), .. } if t == 114.8));
        assert_eq!(plan.entries[1].tag, "Car to car");
        assert!(matches!(plan.entries[2].kind, Kind::Digital { nac: Some(0x293), .. }));
    }

    #[test]
    fn request_escapes_credentials() {
        let creds = Credentials {
            username: "a<b".into(),
            password: "p&w".into(),
            app_key: "k".into(),
        };
        let xml = envelope("getTrsTalkgroups", "sid", 6919, &creds);
        assert!(xml.contains("<username xsi:type=\"xsd:string\">a&lt;b</username>"));
        assert!(xml.contains(">p&amp;w<") && xml.contains("<sid xsi:type=\"xsd:int\">6919</sid><tgCid"));
        Document::parse(xml.split_once("?>").unwrap().1).unwrap();
    }
}
