//! Input discovery and Ruby-compatible output behavior through the public API.

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use tahweel::{DocumentKind, Format, Input, OutputWriter};

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "z.pdf",
        "a/02.JPEG",
        "a/deeper/01.pNg",
        "notes.txt",
        "out/old.jpg",
    ] {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"not checked during folder discovery").unwrap();
    }
    dir
}

#[test]
fn folders_are_sorted_recursive_and_exclude_nested_output() {
    let dir = fixture();
    let input = Input::from(dir.path());
    let relative = |recursive, excluded: Option<&Path>| {
        input
            .discover(recursive, excluded)
            .unwrap()
            .into_iter()
            .map(|document| document.path.strip_prefix(dir.path()).unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(relative(false, None), [Path::new("z.pdf")]);
    assert_eq!(
        relative(true, None),
        ["a/02.JPEG", "a/deeper/01.pNg", "out/old.jpg", "z.pdf"].map(Path::new)
    );
    assert_eq!(
        relative(true, Some(&dir.path().join("a/../out"))),
        ["a/02.JPEG", "a/deeper/01.pNg", "z.pdf"].map(Path::new)
    );
    assert_eq!(relative(true, Some(dir.path())).len(), 4);
    assert_eq!(
        relative(true, Some(&dir.path().join("not-created"))).len(),
        4
    );
}

#[test]
fn explicit_files_use_magic_instead_of_extension() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes, kind) in [
        ("pdf.data", b"%PDF-1.7".as_slice(), DocumentKind::Pdf),
        (
            "jpeg.pdf",
            b"\xff\xd8\xff\xe0".as_slice(),
            DocumentKind::Image,
        ),
        ("png", b"\x89PNG\r\n\x1a\n".as_slice(), DocumentKind::Image),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        let documents = Input::from(&path).discover(false, None).unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].kind, kind);
        assert_eq!(documents[0].title, name);
    }
    for (name, bytes) in [
        ("bad.pdf", b"text".as_slice()),
        ("empty.png", b"".as_slice()),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        assert!(
            Input::from(path)
                .discover(false, None)
                .unwrap_err()
                .to_string()
                .contains(name)
        );
    }
    assert!(
        Input::from(dir.path().join("missing"))
            .discover(false, None)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn explicit_directory_links_work_without_following_nested_directory_links() {
    let dir = fixture();
    std::os::unix::fs::symlink(dir.path(), dir.path().join("a/loop")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("z.pdf"), dir.path().join("linked.pdf")).unwrap();
    let documents = Input::from(dir.path()).discover(true, None).unwrap();
    assert_eq!(documents.len(), 5);
    assert!(documents.iter().any(|document| document.id == "linked"));
    let alias = dir.path().join("a/loop");
    let through_alias = Input::from(&alias).discover(true, None).unwrap();
    assert_eq!(through_alias.len(), 5);
    assert!(
        through_alias
            .iter()
            .all(|document| document.path.starts_with(&alias))
    );
}

fn text(format: Format, pages: &[&str]) -> String {
    String::from_utf8(
        OutputWriter::new()
            .render(
                format,
                &pages.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            )
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn txt_matches_ruby_separator_and_trimming_with_lf_endings() {
    insta::assert_snapshot!(text(Format::Txt, &["  الصفحة الأولى\r\nسطر ثانٍ \r\n", " Second page \n"]), @r"
    الصفحة الأولى
    سطر ثانٍ

    PAGE_SEPARATOR

    Second page
    ");
    let rendered = text(Format::Txt, &[" x ", "y"]);
    assert_eq!(rendered.as_bytes(), b"x\n\nPAGE_SEPARATOR\n\ny");
    assert_eq!(text(Format::Txt, &[]), "");
    let custom = OutputWriter::new().page_separator("\r\n---\r\n");
    assert_eq!(
        custom
            .render(Format::Txt, &[" a ".into(), " b ".into()])
            .unwrap(),
        b"a\n---\nb"
    );
}

#[test]
fn json_matches_ruby_shape_order_indentation_and_trimming() {
    insta::assert_snapshot!(text(Format::Json, &["  العربية\nسطر  ", "\tSecond \"page\"\t"]), @r#"
    [
      {
        "page": 1,
        "content": "العربية\nسطر"
      },
      {
        "page": 2,
        "content": "Second \"page\""
      }
    ]
    "#);
    assert_eq!(text(Format::Json, &[]), "[]");
    assert!(text(Format::Json, &[" x "]).ends_with(']'));
    assert!(text(Format::Json, &["a\r\nb"]).contains("a\\nb"));
    let pages = [" \r\na\rb\r\nc \t", "\t العربية\r\n "];
    let json: serde_json::Value = serde_json::from_str(&text(Format::Json, &pages)).unwrap();
    let contents: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|page| page["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        text(Format::Txt, &pages),
        contents.join("\n\nPAGE_SEPARATOR\n\n")
    );
}

fn document_xml(pages: &[&str]) -> String {
    docx_part(pages, "word/document.xml")
}

fn docx_part(pages: &[&str], part: &str) -> String {
    let bytes = OutputWriter::new()
        .render(
            Format::Docx,
            &pages.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    archive
        .by_name(part)
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn paragraphs(xml: &str) -> Vec<(String, String, bool, usize)> {
    let document = roxmltree::Document::parse(xml).unwrap();
    document
        .descendants()
        .filter(|node| node.has_tag_name((W, "p")))
        .filter(|node| node.descendants().any(|child| child.has_tag_name((W, "t"))))
        .map(|node| {
            let text = node
                .descendants()
                .filter(|child| child.has_tag_name((W, "t")))
                .map(|child| child.text().unwrap_or(""))
                .collect::<String>();
            let align = node
                .descendants()
                .find(|child| child.has_tag_name((W, "jc")))
                .and_then(|child| child.attribute((W, "val")))
                .unwrap()
                .to_owned();
            let rtl = node
                .descendants()
                .any(|child| child.has_tag_name((W, "bidi")));
            let breaks = node
                .descendants()
                .filter(|child| child.has_tag_name((W, "br")))
                .count();
            (text, align, rtl, breaks)
        })
        .collect()
}

#[test]
fn docx_ports_ruby_whitespace_line_break_and_page_break_cases() {
    let xml = document_xml(&[
        "Page  1   content",
        "Page\r\n\r\n2\t\tcontent",
        "Line 1\r\nLine 2\rLine 3",
    ]);
    let pages = paragraphs(&xml);
    assert_eq!(pages[0], ("Page 1 content".into(), "left".into(), false, 0));
    assert_eq!(pages[1], ("Page2content".into(), "left".into(), false, 1));
    assert_eq!(
        pages[2],
        ("Line 1Line 2Line 3".into(), "left".into(), false, 2)
    );
    let document = roxmltree::Document::parse(&xml).unwrap();
    assert_eq!(
        document
            .descendants()
            .filter(|node| node.has_tag_name((W, "tab")))
            .count(),
        1
    );
    let breaks: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name((W, "br")) && node.attribute((W, "type")) == Some("page"))
        .collect();
    assert_eq!(breaks.len(), 2);
    let body = document
        .descendants()
        .find(|node| node.has_tag_name((W, "body")))
        .unwrap();
    let last_paragraph = body
        .children()
        .rfind(|node| node.has_tag_name((W, "p")))
        .unwrap();
    assert!(
        !last_paragraph
            .descendants()
            .any(|node| node.attribute((W, "type")) == Some("page"))
    );
    assert!(xml.contains("<w:br/>"));
}

#[test]
fn docx_body_text_is_ten_points_like_ruby() {
    let xml = docx_part(&["بسم الله"], "word/styles.xml");
    let styles = roxmltree::Document::parse(&xml).unwrap();
    let defaults = styles
        .descendants()
        .find(|node| node.has_tag_name((W, "rPrDefault")))
        .unwrap();
    for size in ["sz", "szCs"] {
        let value = defaults
            .descendants()
            .find(|node| node.has_tag_name((W, size)))
            .and_then(|node| node.attribute((W, "val")));
        assert_eq!(value, Some("20"), "w:{size} is in half points");
    }
}

#[test]
fn docx_alignment_matches_ruby_character_counts_and_explicit_direction() {
    let xml = document_xml(&[
        "مرحبا بكم",
        "مرحبا hello",
        "hello مرحب",
        "123 !!!",
        "a ١٢٣٤٥ ،؛؟",
        "English",
        "x العربية العربية",
        "ب 😀😀",
        "ب ََ",
        "ب ــ",
        "ؐؐ x",
        "ب\u{00a0}x",
        "\u{00a0}بx",
    ]);
    let pages = paragraphs(&xml);
    assert_eq!(
        pages
            .iter()
            .map(|p| (p.1.as_str(), p.2))
            .collect::<Vec<_>>(),
        [
            ("right", true),
            ("right", true),
            ("left", false),
            ("right", true),
            ("right", true),
            ("left", false),
            ("right", true),
            ("left", false),
            ("left", false),
            ("left", false),
            ("right", true),
            ("left", false),
            ("left", false),
        ]
    );
}

#[test]
fn docx_compacts_45_short_lines_and_accounts_for_wrapped_lines() {
    let short = vec!["a"; 45].join("\n");
    let xml = document_xml(&[&short]);
    let pages = paragraphs(&xml);
    assert_eq!(pages[0].3, 39);
    assert!(pages[0].0.starts_with("a aa aa aa aa a"));
    let long = "ب".repeat(81);
    let mixed = std::iter::once(long.as_str())
        .chain(std::iter::repeat_n("a", 39))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(paragraphs(&document_xml(&[&mixed]))[0].3, 38);
    assert_eq!(paragraphs(&document_xml(&["Just one line"]))[0].3, 0);
    assert!(document_xml(&[""]).contains("<w:p>"));
}

#[cfg(unix)]
#[test]
fn output_permissions_match_normal_files_under_the_process_umask() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let reference = dir.path().join("normal-file");
    fs::File::create(&reference).unwrap();
    let expected = fs::metadata(reference).unwrap().permissions().mode() & 0o777;
    let outputs = OutputWriter::new()
        .write(dir.path(), "book", &["page".into()])
        .unwrap();
    for output in outputs {
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            expected
        );
    }
}

#[test]
fn writer_replaces_outputs_and_cleans_temporary_files_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let writer = OutputWriter::new().formats([Format::Txt, Format::Json, Format::Txt]);
    assert!(!writer.exists(dir.path(), "book"));
    let paths = writer.write(dir.path(), "book", &["old".into()]).unwrap();
    assert_eq!(paths.len(), 2);
    assert!(writer.exists(dir.path(), "book"));
    writer.write(dir.path(), "book", &["new".into()]).unwrap();
    assert_eq!(fs::read(&paths[0]).unwrap(), b"new");
    // A DOCX render failure occurs before any staged output is published.
    assert!(
        OutputWriter::new()
            .write(dir.path(), "book", &["bad\0text".into()])
            .is_err()
    );
    assert_eq!(fs::read(&paths[0]).unwrap(), b"new");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    assert!(!OutputWriter::new().exists(dir.path(), "book"));
    let memory_only = OutputWriter::new().formats([]);
    assert!(!memory_only.exists(dir.path(), "book"));
    assert_eq!(
        memory_only
            .write(&dir.path().join("unused"), "book", &[])
            .unwrap(),
        Vec::<std::path::PathBuf>::new()
    );
    assert!(!dir.path().join("unused").exists());
}
