//! A small element tree over quick-xml, which the EWS answers are read from
//! (PERSO-256).
//!
//! An answer is a few kilobytes to a few hundred, read once, so a tree is
//! simpler to walk than a stream and costs nothing that matters. Elements are
//! named by their local name: EWS answers mix the `m:` and `t:` prefixes with
//! default namespaces (GetUserAvailability, GetReminders), and no two
//! elements an answer is read by share a local name across namespaces.
//!
//! A document that is not well formed, or ends before its root closes (an
//! answer cut short), is an error, never an empty tree.

use std::borrow::Cow;

use anyhow::{anyhow, bail, Result};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// One element: its local name, its attributes by local name, its child
/// elements in order, and its own text (character data and references
/// resolved, child elements' text excluded).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    /// The first child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    /// Every child named `name`, in order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// The descendant at `path`, one child name per step.
    pub fn at(&self, path: &[&str]) -> Option<&Element> {
        path.iter()
            .try_fold(self, |element, name| element.child(name))
    }

    /// The attribute `name`, by local name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The trimmed text of the child `name`; `None` when there is no such
    /// child or its text is empty.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name)
            .map(|child| child.text.trim())
            .filter(|text| !text.is_empty())
    }

    /// The child `name` read as an XML Schema boolean (`true`, `false`, `1`,
    /// `0`); `None` when absent or unreadable.
    pub fn flag(&self, name: &str) -> Option<bool> {
        match self.text_of(name)? {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }
}

/// The local name of a tag or attribute key, `t:CalendarItem` read as
/// `CalendarItem`.
fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

fn element_of(start: &BytesStart<'_>) -> Result<Element> {
    let mut element = Element {
        name: local(start.name().as_ref()),
        ..Default::default()
    };
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|err| anyhow!("unreadable attribute: {err}"))?;
        let key = attribute.key.as_ref();
        // Namespace declarations are not attributes an answer is read by.
        if key == "xmlns" || key.starts_with("xmlns:") {
            continue;
        }
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| anyhow!("unreadable attribute value: {err}"))?;
        element.attributes.push((local(key), value.into_owned()));
    }
    Ok(element)
}

/// The text a general reference (`&amp;`, `&#xD;`) stands for.
fn reference_text(name: &str) -> Result<Cow<'static, str>> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number
            .strip_prefix('x')
            .or_else(|| number.strip_prefix('X'))
        {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => number.parse(),
        }
        .map_err(|_| anyhow!("bad character reference &{name};"))?;
        let ch = char::from_u32(code).ok_or_else(|| anyhow!("bad character reference &{name};"))?;
        return Ok(Cow::Owned(ch.to_string()));
    }
    resolve_predefined_entity(name)
        .map(Cow::Borrowed)
        .ok_or_else(|| anyhow!("unknown entity &{name};"))
}

/// The root element of `xml`.
pub fn parse(xml: &str) -> Result<Element> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = true;
    let mut open: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let event = reader.read_event().map_err(|err| {
            anyhow!(
                "not well-formed XML at byte {}: {err}",
                reader.buffer_position()
            )
        })?;
        match event {
            Event::Start(start) => {
                if root.is_some() {
                    bail!("content after the root element");
                }
                open.push(element_of(&start)?);
            }
            Event::Empty(start) => {
                let element = element_of(&start)?;
                match open.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None if root.is_none() => root = Some(element),
                    None => bail!("content after the root element"),
                }
            }
            Event::End(_) => {
                let element = open
                    .pop()
                    .ok_or_else(|| anyhow!("a closing tag closes nothing"))?;
                match open.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => root = Some(element),
                }
            }
            Event::Text(text) => {
                if let Some(element) = open.last_mut() {
                    element.text.push_str(&text.xml10_content());
                } else if !text.xml10_content().trim().is_empty() {
                    bail!("text outside the root element");
                }
            }
            Event::CData(data) => {
                let element = open
                    .last_mut()
                    .ok_or_else(|| anyhow!("CDATA outside the root element"))?;
                element.text.push_str(data.as_ref());
            }
            Event::GeneralRef(reference) => {
                let element = open
                    .last_mut()
                    .ok_or_else(|| anyhow!("a reference outside the root element"))?;
                element.text.push_str(&reference_text(&reference)?);
            }
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }
    if let Some(unclosed) = open.last() {
        bail!("the answer ends inside <{}>", unclosed.name);
    }
    root.ok_or_else(|| anyhow!("the answer holds no XML element"))
}

/// `text` escaped for an XML attribute value or character data.
pub fn escape(text: &str) -> Cow<'_, str> {
    quick_xml::escape::escape(text)
}
