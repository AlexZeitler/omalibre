//! Reading EPUB containers: package document, spine and navigation.

pub mod xhtml;

use crate::doc::Chapter;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Metadata taken from the package document. Only fields the reader shows.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    /// `dc:identifier`, used later to match two files of the same book.
    pub identifier: Option<String>,
}

/// One entry of the reading order.
#[derive(Debug, Clone)]
pub struct SpineItem {
    /// Path inside the container, relative to its root.
    pub href: String,
    /// Title from the navigation document, when one points here.
    pub title: Option<String>,
}

pub struct Book {
    archive: ZipArchive<File>,
    /// Directory of the package document; hrefs resolve against it.
    root: PathBuf,
    pub metadata: Metadata,
    pub spine: Vec<SpineItem>,
}

impl Book {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let mut archive = ZipArchive::new(file)
            .with_context(|| format!("{} is not a zip archive", path.display()))?;

        let opf_path = find_package_path(&mut archive)?;
        let root = Path::new(&opf_path)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();

        let opf = read_entry(&mut archive, &opf_path)?;
        let package = parse_package(&opf)?;

        let mut spine = Vec::new();
        for idref in &package.spine {
            if let Some(href) = package.manifest.get(idref) {
                spine.push(SpineItem {
                    href: normalize(&root, href),
                    title: None,
                });
            }
        }
        if spine.is_empty() {
            bail!("package document lists no readable spine items");
        }

        let mut book = Self {
            archive,
            root,
            metadata: package.metadata,
            spine,
        };
        book.apply_navigation(&package.nav_href, &package.ncx_href);
        Ok(book)
    }

    /// Parses the chapter at `index` of the reading order.
    pub fn chapter(&mut self, index: usize) -> Result<Chapter> {
        let item = self
            .spine
            .get(index)
            .ok_or_else(|| anyhow!("no spine item at index {index}"))?
            .clone();
        let source = read_entry(&mut self.archive, &item.href)?;
        // Image paths in a chapter are relative to the chapter's own directory.
        let base = Path::new(&item.href)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = xhtml::parse_in(&source, &base)
            .with_context(|| format!("cannot parse chapter {}", item.href))?;
        Ok(Chapter {
            href: item.href,
            blocks: parsed.blocks,
            links: parsed.links,
            anchors: parsed.anchors,
        })
    }

    pub fn title(&self) -> &str {
        self.metadata.title.as_deref().unwrap_or("Untitled")
    }

    /// Raw bytes of a file inside the container, for images.
    pub fn read_binary(&mut self, name: &str) -> Result<Vec<u8>> {
        let mut entry = self
            .archive
            .by_name(name)
            .with_context(|| format!("{name} is missing from the container"))?;
        let mut buffer = Vec::new();
        entry.read_to_end(&mut buffer)?;
        Ok(buffer)
    }

    /// Fills in spine titles from the navigation document, preferring EPUB 3's
    /// nav over EPUB 2's NCX.
    fn apply_navigation(&mut self, nav_href: &Option<String>, ncx_href: &Option<String>) {
        let titles = nav_href
            .as_ref()
            .and_then(|href| {
                let full = normalize(&self.root.clone(), href);
                read_entry(&mut self.archive, &full)
                    .ok()
                    .and_then(|xml| parse_nav(&xml, &full).ok())
            })
            .or_else(|| {
                ncx_href.as_ref().and_then(|href| {
                    let full = normalize(&self.root.clone(), href);
                    read_entry(&mut self.archive, &full)
                        .ok()
                        .and_then(|xml| parse_ncx(&xml, &full).ok())
                })
            });

        let Some(titles) = titles else { return };
        for item in &mut self.spine {
            if let Some(title) = titles.get(&item.href) {
                item.title = Some(title.clone());
            }
        }
    }
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("{name} is missing from the container"))?;
    let mut buffer = Vec::new();
    entry.read_to_end(&mut buffer)?;
    Ok(decode(buffer))
}

/// Decodes bytes as UTF-8, replacing invalid sequences rather than failing.
fn decode(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    }
}

/// Resolves an href from the package document against the container root and
/// strips any fragment.
fn normalize(root: &Path, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let decoded = percent_decode(href);
    let joined = if root.as_os_str().is_empty() {
        PathBuf::from(&decoded)
    } else {
        root.join(&decoded)
    };

    // Collapse `.` and `..` without touching the filesystem.
    let joined = joined.to_string_lossy().into_owned();
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_package_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let container = read_entry(archive, "META-INF/container.xml")
        .context("not an EPUB: META-INF/container.xml is missing")?;
    let doc = roxmltree::Document::parse_with_options(&container, xhtml::parsing_options())
        .context("container.xml is malformed")?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .map(|p| percent_decode(p))
        .ok_or_else(|| anyhow!("container.xml names no package document"))
}

struct Package {
    metadata: Metadata,
    /// Manifest item id to href, as written in the package document.
    manifest: HashMap<String, String>,
    /// Spine idrefs in reading order.
    spine: Vec<String>,
    nav_href: Option<String>,
    ncx_href: Option<String>,
}

fn parse_package(xml: &str) -> Result<Package> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("package document is malformed")?;
    let mut metadata = Metadata::default();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut nav_href = None;
    let mut ncx_href = None;
    let mut spine_toc_id = None;

    for node in doc.descendants().filter(|n| n.is_element()) {
        let name = node.tag_name().name();
        match name {
            "title" if metadata.title.is_none() => metadata.title = text_of(node),
            "creator" => {
                if let Some(text) = text_of(node) {
                    metadata.authors.push(text);
                }
            }
            "language" if metadata.language.is_none() => metadata.language = text_of(node),
            "identifier" if metadata.identifier.is_none() => metadata.identifier = text_of(node),
            "item" => {
                let id = node.attribute("id");
                let href = node.attribute("href");
                if let (Some(id), Some(href)) = (id, href) {
                    let properties = node.attribute("properties").unwrap_or_default();
                    if properties.split_whitespace().any(|p| p == "nav") {
                        nav_href = Some(href.to_string());
                    }
                    if node.attribute("media-type") == Some("application/x-dtbncx+xml") {
                        ncx_href = Some(href.to_string());
                    }
                    manifest.insert(id.to_string(), href.to_string());
                }
            }
            "spine" => {
                spine_toc_id = node.attribute("toc").map(str::to_string);
            }
            "itemref" => {
                if let Some(idref) = node.attribute("idref") {
                    // `linear="no"` marks material outside the reading order.
                    if node.attribute("linear") != Some("no") {
                        spine.push(idref.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // EPUB 2 points at the NCX through the spine's `toc` attribute.
    if ncx_href.is_none() {
        if let Some(id) = spine_toc_id {
            ncx_href = manifest.get(&id).cloned();
        }
    }

    Ok(Package {
        metadata,
        manifest,
        spine,
        nav_href,
        ncx_href,
    })
}

fn text_of(node: roxmltree::Node) -> Option<String> {
    let text: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Maps chapter paths to titles, taken from an EPUB 3 navigation document.
fn parse_nav(xml: &str, nav_path: &str) -> Result<HashMap<String, String>> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("navigation document is malformed")?;
    let base = Path::new(nav_path).parent().unwrap_or(Path::new(""));
    let mut titles = HashMap::new();

    let toc = doc
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "nav"
                && n.attributes()
                    .any(|a| a.name() == "type" && a.value() == "toc")
        })
        .or_else(|| {
            doc.descendants()
                .find(|n| n.is_element() && n.tag_name().name() == "nav")
        });
    let Some(toc) = toc else { return Ok(titles) };

    for anchor in toc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "a")
    {
        if let (Some(href), Some(title)) = (anchor.attribute("href"), text_of(anchor)) {
            titles
                .entry(normalize(base, href))
                .or_insert(title);
        }
    }
    Ok(titles)
}

/// Maps chapter paths to titles, taken from an EPUB 2 NCX document.
fn parse_ncx(xml: &str, ncx_path: &str) -> Result<HashMap<String, String>> {
    let doc = roxmltree::Document::parse_with_options(xml, xhtml::parsing_options())
        .context("NCX document is malformed")?;
    let base = Path::new(ncx_path).parent().unwrap_or(Path::new(""));
    let mut titles = HashMap::new();

    for point in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "navPoint")
    {
        let href = point
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "content")
            .and_then(|n| n.attribute("src"));
        let title = point
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "text")
            .and_then(text_of);
        if let (Some(href), Some(title)) = (href, title) {
            titles.entry(normalize(base, href)).or_insert(title);
        }
    }
    Ok(titles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_relative_hrefs() {
        assert_eq!(normalize(Path::new("OEBPS"), "ch01.xhtml"), "OEBPS/ch01.xhtml");
        assert_eq!(normalize(Path::new("OEBPS/text"), "../ch01.xhtml"), "OEBPS/ch01.xhtml");
        assert_eq!(normalize(Path::new(""), "ch01.xhtml"), "ch01.xhtml");
    }

    #[test]
    fn strips_fragments_and_decodes_escapes() {
        assert_eq!(normalize(Path::new(""), "ch01.xhtml#part2"), "ch01.xhtml");
        assert_eq!(normalize(Path::new(""), "a%20b.xhtml"), "a b.xhtml");
    }

    #[test]
    fn reads_metadata_and_reading_order() {
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title>A Book</dc:title>
            <dc:creator>An Author</dc:creator>
            <dc:language>en</dc:language>
            <dc:identifier>urn:uuid:1234</dc:identifier>
          </metadata>
          <manifest>
            <item id="c1" href="ch01.xhtml" media-type="application/xhtml+xml"/>
            <item id="c2" href="ch02.xhtml" media-type="application/xhtml+xml"/>
            <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
            <item id="nav" href="nav.xhtml" properties="nav" media-type="application/xhtml+xml"/>
          </manifest>
          <spine>
            <itemref idref="c1"/>
            <itemref idref="cover" linear="no"/>
            <itemref idref="c2"/>
          </spine>
        </package>"#;
        let package = parse_package(opf).unwrap();
        assert_eq!(package.metadata.title.as_deref(), Some("A Book"));
        assert_eq!(package.metadata.authors, vec!["An Author"]);
        assert_eq!(package.metadata.identifier.as_deref(), Some("urn:uuid:1234"));
        assert_eq!(package.spine, vec!["c1", "c2"]);
        assert_eq!(package.nav_href.as_deref(), Some("nav.xhtml"));
    }

    #[test]
    fn reads_titles_from_nav_document() {
        let nav = r#"<html xmlns:epub="http://www.idpf.org/2007/ops">
          <body><nav epub:type="toc"><ol>
            <li><a href="ch01.xhtml">First</a></li>
            <li><a href="ch02.xhtml#top">Second</a></li>
          </ol></nav></body></html>"#;
        let titles = parse_nav(nav, "OEBPS/nav.xhtml").unwrap();
        assert_eq!(titles.get("OEBPS/ch01.xhtml").map(String::as_str), Some("First"));
        assert_eq!(titles.get("OEBPS/ch02.xhtml").map(String::as_str), Some("Second"));
    }

    #[test]
    fn reads_titles_from_ncx() {
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
          <navPoint><navLabel><text>Chapter One</text></navLabel>
            <content src="ch01.xhtml"/></navPoint>
        </navMap></ncx>"#;
        let titles = parse_ncx(ncx, "OEBPS/toc.ncx").unwrap();
        assert_eq!(titles.get("OEBPS/ch01.xhtml").map(String::as_str), Some("Chapter One"));
    }
}
