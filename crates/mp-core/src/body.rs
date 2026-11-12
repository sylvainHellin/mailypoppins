//! The plain-text body as a reader sees it (PERSO-291).
//!
//! A sender's client can put hundreds of blank lines above the text (a
//! Thunderbird `format=flowed` mail opened with 314), and a body shown
//! verbatim then reads as empty, its text sitting off-screen. Every surface
//! that shows a body (`mp show`, the search output, the TUI preview, the
//! desktop reader) and the quote of a reply or a forward read it through
//! [`for_reading`]; the stored body and the raw message stay as sent, so a
//! mail stored before the rule existed reads cleaned too.

/// `body` without its leading and trailing blank lines, each run of more than
/// two blank lines inside it collapsed to one.
///
/// A line is blank when it holds only whitespace: quoted-printable `=20`
/// leaves a line with a single space, which reads as blank and is one. A
/// quoted line such as `>` is text. Every kept line keeps its own ending, so
/// a CRLF body stays CRLF, and a run of one or two blank lines stays as it
/// is; a collapsed run keeps its first line. A body of nothing but blank
/// lines reads as the empty string.
pub fn for_reading(body: &str) -> String {
    let blank = |line: &str| line.trim().is_empty();
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let Some(first) = lines.iter().position(|line| !blank(line)) else {
        return String::new();
    };
    let last = lines.iter().rposition(|line| !blank(line)).unwrap_or(first);

    let mut out = String::with_capacity(body.len());
    let mut run: Vec<&str> = Vec::new();
    for line in &lines[first..=last] {
        if blank(line) {
            run.push(line);
            continue;
        }
        match run.len() {
            0..=2 => run.iter().for_each(|kept| out.push_str(kept)),
            _ => out.push_str(run[0]),
        }
        run.clear();
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::for_reading;

    #[test]
    fn leading_blank_lines_are_dropped() {
        assert_eq!(
            for_reading("\n\n\nDear Ms Example,\n"),
            "Dear Ms Example,\n"
        );
    }

    #[test]
    fn trailing_blank_lines_are_dropped_and_the_last_line_keeps_its_ending() {
        assert_eq!(for_reading("Regards\n\n\n\n"), "Regards\n");
        assert_eq!(for_reading("Regards\n  \n\t\n"), "Regards\n");
        assert_eq!(for_reading("Regards"), "Regards");
    }

    #[test]
    fn two_blank_lines_inside_the_body_stay() {
        assert_eq!(
            for_reading("one\n\ntwo\n\n\nthree\n"),
            "one\n\ntwo\n\n\nthree\n"
        );
    }

    #[test]
    fn three_blank_lines_inside_the_body_collapse_to_one() {
        assert_eq!(for_reading("one\n\n\n\ntwo\n"), "one\n\ntwo\n");
        assert_eq!(for_reading("one\n\n\n\n\n\n\n\ntwo\n"), "one\n\ntwo\n");
    }

    #[test]
    fn a_whitespace_only_line_is_blank() {
        assert_eq!(for_reading(" \n\t\nText\n"), "Text\n");
        assert_eq!(for_reading("one\n \n\t\n   \ntwo\n"), "one\n \ntwo\n");
        // A run of two keeps its lines as they are, spaces included.
        assert_eq!(for_reading("one\n \n\t\ntwo\n"), "one\n \n\t\ntwo\n");
    }

    #[test]
    fn a_quoted_line_is_text() {
        assert_eq!(for_reading(">\n>\n>\n>\nquoted\n"), ">\n>\n>\n>\nquoted\n");
        assert_eq!(for_reading("> \n\n\n\n> text\n"), "> \n\n> text\n");
    }

    #[test]
    fn a_crlf_body_keeps_its_line_endings() {
        assert_eq!(
            for_reading("\r\n \r\nDear\r\n\r\n\r\n\r\n\r\nthanks\r\n\r\n"),
            "Dear\r\n\r\nthanks\r\n"
        );
        assert_eq!(
            for_reading("one\r\n\r\n\r\ntwo\r\n"),
            "one\r\n\r\n\r\ntwo\r\n"
        );
    }

    #[test]
    fn a_body_of_only_blank_lines_reads_as_empty() {
        assert_eq!(for_reading("\r\n \r\n\n\t\n"), "");
        assert_eq!(for_reading(""), "");
    }

    #[test]
    fn indentation_of_the_first_line_stays() {
        assert_eq!(for_reading("\n\n    indented\n"), "    indented\n");
    }
}
