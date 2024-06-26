//! SRT-format subtitle support.

use std::{fs::File, io::Read as _, path::Path};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::{
    clean::{clean_subtitle_file, strip_formatting},
    decode::smart_decode,
    lang::Lang,
    time::Period,
    Result,
};

/// Format seconds using the standard SRT time format.
pub fn format_time(time: f32) -> String {
    let (h, rem) = ((time / 3600.0).trunc(), time % 3600.0);
    let (m, s) = ((rem / 60.0).trunc(), rem % 60.0);
    (format!("{:02}:{:02}:{:0>6.3}", h, m, s)).replace(".", ",")
}

/// A single SRT-format subtitle, minus some of the optional fields used in
/// various versions of the file format.
#[derive(Debug, PartialEq, Clone, Deserialize, Serialize)]
pub struct Subtitle {
    /// The index of this subtitle.  We should normalize these to start
    /// with 1 on output.
    pub index: usize,

    /// The time period during which this subtitle is shown.
    pub period: Period,

    /// The lines of text in this subtitle.
    pub lines: Vec<String>,
}

impl Subtitle {
    /// Return a string representation of this subtitle.
    pub fn to_string(&self) -> String {
        format!(
            "{}\n{} --> {}\n{}\n",
            self.index,
            format_time(self.period.begin()),
            format_time(self.period.end()),
            self.lines.join("\n")
        )
    }

    /// Return a plain-text version of this subtitle.
    pub fn plain_text(&self) -> String {
        strip_formatting(&self.lines.join(" ")).into_owned()
    }
}

/// The contents of an SRT-format subtitle file.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SubtitleFile {
    /// The subtitles in this file.
    pub subtitles: Vec<Subtitle>,
}

impl SubtitleFile {
    /// Parse raw subtitle text into an appropriate structure.
    pub fn from_str(data: &str) -> Result<SubtitleFile> {
        // Use `trim_left_matches` to remove the leading BOM ("byte order mark")
        // that's present in much Windows UTF-8 data. Note that if it appears
        // multiple times, we would remove all the copies, but we've never seen
        // that in the wild.
        Ok(grammar::subtitle_file(data.trim_start_matches("\u{FEFF}"))
            .context("could not parse subtitles")?)
    }

    /// Parse the subtitle file found at the specified path.
    pub fn from_path(path: &Path) -> Result<SubtitleFile> {
        let mut file = File::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .with_context(|| format!("could not read {}", path.display()))?;
        let data = smart_decode(&bytes)
            .with_context(|| format!("could not read {}", path.display()))?;
        Ok(SubtitleFile::from_str(&data)
            .with_context(|| format!("could not parse {}", path.display()))?)
    }

    /// Parse and normalize the subtitle file found at the specified path.
    pub fn cleaned_from_path(path: &Path) -> Result<SubtitleFile> {
        let raw = SubtitleFile::from_path(path)?;
        Ok(clean_subtitle_file(&raw)?)
    }

    /// Convert subtitles to a string.
    pub fn to_string(&self) -> String {
        let subs: Vec<String> = self.subtitles.iter().map(|s| s.to_string()).collect();
        // The BOM (byte-order mark) is generally discouraged on Linux, but
        // it's sometimes needed to get good results under Windows.  We
        // include it here because Wikipedia says that SRT files files
        // default to various legacy encoding, but that the BOM can be used
        // for Unicode.
        format!("\u{FEFF}{}", subs.join("\n"))
    }

    /// Find the subtitle with the given index.
    pub fn find(&self, index: usize) -> Option<&Subtitle> {
        self.subtitles.iter().find(|s| s.index == index)
    }

    /// Detect the language used in these subtitles.
    pub fn detect_language(&self) -> Option<Lang> {
        let subs: Vec<_> = self.subtitles.iter().map(|s| s.plain_text()).collect();
        let text = subs.join("\n");
        Lang::for_text(&text)
    }
}

/// Interface for time-based formats that can be appended with an offset.
pub trait AppendWithOffset {
    /// Append another file, shifting it by the specified time offset.
    /// We use this to reassamble transcription segments.
    fn append_with_offset(&mut self, other: Self, time_offset: f32);
}

impl AppendWithOffset for SubtitleFile {
    fn append_with_offset(&mut self, mut other: SubtitleFile, time_offset: f32) {
        // Renumber indices in the first file starting from 1.
        let mut next_index = 1;
        for sub in &mut self.subtitles {
            sub.index = next_index;
            next_index += 1;
        }

        // Renumber indices in the second file starting from the next index, and
        // shift the time periods by the specified offset.
        for sub in &mut other.subtitles {
            sub.index = next_index;
            next_index += 1;
            sub.period = sub.period.shift(time_offset);
        }

        // Append the second file to the first.
        self.subtitles.extend(other.subtitles);
    }
}

peg::parser! {
    grammar grammar() for str {
        use std::str::FromStr;

        use super::{Subtitle, SubtitleFile};
        use crate::time::Period;

        pub rule subtitle_file() -> SubtitleFile
            = blank_lines()? result:subtitles() blank_lines()? {
                SubtitleFile { subtitles: result }
            }

        rule subtitles() -> Vec<Subtitle>
            = subs:subtitle() ** blank_lines() { subs }

        rule subtitle() -> Subtitle
            = index:digits() newline() p:time_period() newline() l:lines() {
                Subtitle { index: index, period: p, lines: l }
            }

        rule time_period() -> Period
            = begin:time() " --> " end:time() {?
                let mut end = end;
                if begin == end {
                    // If subtitle has zero length, fix it. These are generated by
                    // the Aeneas audio/text alignment tool, which is otherwise
                    // excellent for use with audiobooks.
                    end += 0.001;
                }
                match Period::new(begin, end) {
                  Ok(p) => Ok(p),
                  Err(_) => Err("invalid time period"),
                }
            }

        rule time() -> f32
            = hh:digits() ":" mm:digits() ":" ss:comma_float() {
                (hh as f32)*3600.0 + (mm as f32)*60.0 + ss
            }

        rule lines() -> Vec<String>
            = lines:line() ** newline() { lines }

        rule line() -> String
            = text:$([^ '\r' | '\n']+) { text.to_string() }

        rule digits() -> usize
            = digits:$(['0'..='9']+) { FromStr::from_str(digits).unwrap() }

        rule comma_float() -> f32
            = float_str:$(['0'..='9']+ "," ['0'..='9']+) {
                let fixed: String = float_str.replace(",", ".");
                FromStr::from_str(&fixed).unwrap()
            }

        rule newline()
            = "\r"? "\n"

        rule blank_lines()
            = newline()+
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use crate::{
        lang::Lang,
        srt::{Subtitle, SubtitleFile},
        time::Period,
    };

    #[test]
    fn subtitle_file_from_path() {
        let path = Path::new("fixtures/sample.es.srt");
        let srt = SubtitleFile::from_path(&path).unwrap();
        assert_eq!(5, srt.subtitles.len());

        let sub = &srt.subtitles[0];
        assert_eq!(16, sub.index);
        assert_eq!(62.328, sub.period.begin());
        assert_eq!(64.664, sub.period.end());
        assert_eq!(vec!["¡Si! ¡Aang ha vuelto!".to_string()], sub.lines);

        let sub2 = &srt.subtitles[2];
        assert_eq!(
            vec![
                "Tu diste la señal a la armada".to_string(),
                "del fuego con la bengala,".to_string(),
            ],
            sub2.lines
        );
    }

    #[test]
    fn subtitle_to_string() {
        let sub = Subtitle {
            index: 4,
            period: Period::new(61.5, 63.75).unwrap(),
            lines: vec!["Line 1".to_string(), "<i>Line 2</i>".to_string()],
        };
        let expected = r"4
00:01:01,500 --> 00:01:03,750
Line 1
<i>Line 2</i>
"
        .to_string();
        assert_eq!(expected, sub.to_string());
    }

    #[test]
    fn subtitle_file_to_string() {
        let data = "\u{FEFF}16
00:01:02,328 --> 00:01:04,664
Line 1.1

17
00:01:12,839 --> 00:01:13,839
Line 2.1
";
        let srt = SubtitleFile::from_str(data).unwrap();
        assert_eq!(data, &srt.to_string());
    }

    #[test]
    fn zero_duration_subtitle() {
        let data = "\u{FEFF}16
00:00:01,000 --> 00:00:01,000
Text
";
        let srt = SubtitleFile::from_str(data).unwrap();
        assert_eq!(srt.subtitles.len(), 1);
        assert_eq!(srt.subtitles[0].period.begin(), 1.0);
        assert_eq!(srt.subtitles[0].period.end(), 1.001);
    }

    #[test]
    fn detect_language() {
        let path_es = Path::new("fixtures/sample.es.srt");
        let srt_es = SubtitleFile::from_path(&path_es).unwrap();
        assert_eq!(Some(Lang::iso639("es").unwrap()), srt_es.detect_language());

        let path_en = Path::new("fixtures/sample.en.srt");
        let srt_en = SubtitleFile::from_path(&path_en).unwrap();
        assert_eq!(Some(Lang::iso639("en").unwrap()), srt_en.detect_language());
    }
}
