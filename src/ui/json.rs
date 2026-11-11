//! Shared terminal-aware JSON output for CLI reports.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context as _, Result};
use console_json::{ColorMode, JsonOptions};
use serde::Serialize;

/// Print a JSON document, pretty and colored on terminals, compact in pipes.
///
/// A nonempty `NO_COLOR` disables terminal colors without disabling indentation.
pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    print_json_with_context(value, DEFAULT_CONTEXT)
}

/// Keep command-specific diagnostics while sharing serialization and output behavior.
pub(crate) struct JsonErrorContext {
    pub serialize: &'static str,
    pub write: &'static str,
    pub flush: &'static str,
}

const DEFAULT_CONTEXT: JsonErrorContext = JsonErrorContext {
    serialize: "failed to serialize JSON output",
    write: "failed to write JSON to stdout",
    flush: "failed to flush JSON output",
};

pub(crate) fn print_json_with_context<T: Serialize>(
    value: &T,
    context: JsonErrorContext,
) -> Result<()> {
    let _pause = super::progress::pause()?;
    let terminal = io::stdout().is_terminal();
    let colors = colors_allowed(
        terminal,
        std::env::var_os("NO_COLOR").as_deref(),
        console::colors_enabled(),
    );
    write_json(&mut io::stdout().lock(), value, terminal, colors, context)
}

fn colors_allowed(terminal: bool, no_color: Option<&std::ffi::OsStr>, supported: bool) -> bool {
    terminal && no_color.is_none_or(|value| value.is_empty()) && supported
}

fn write_json<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    terminal: bool,
    colors: bool,
    context: JsonErrorContext,
) -> Result<()> {
    // Finish serialization before writing so failures cannot leave a partial document.
    let json = format_json(value, terminal, colors).context(context.serialize)?;
    writer.write_all(&json).context(context.write)?;
    writer.write_all(b"\n").context(context.write)?;
    writer.flush().context(context.flush)
}

fn format_json<T: Serialize>(value: &T, terminal: bool, colors: bool) -> Result<Vec<u8>> {
    if terminal {
        let mut json = Vec::new();
        console_json::write_json_with_options(
            &mut json,
            value,
            JsonOptions::default().color(if colors {
                ColorMode::Always
            } else {
                ColorMode::Never
            }),
        )?;
        Ok(json)
    } else {
        // Bypass all color policy in pipes, even when the environment forces colors.
        Ok(serde_json::to_vec(value)?)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::{self, Write};

    use rstest::rstest;
    use serde::ser::{Serialize, SerializeSeq, Serializer};

    use super::{DEFAULT_CONTEXT, JsonErrorContext, colors_allowed, format_json, write_json};

    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
        flushed: bool,
        fail_write: bool,
        fail_flush: bool,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::other("write failed"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }

    struct LateSerializationFailure;

    impl Serialize for LateSerializationFailure {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            // Emit a valid prefix to catch accidental streaming to the destination.
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element("already serialized")?;
            Err(serde::ser::Error::custom("late serialization failure"))
        }
    }

    #[rstest]
    #[case(false, None, true, false)]
    #[case(true, None, true, true)]
    #[case(true, Some(""), true, true)]
    #[case(true, Some("1"), true, false)]
    #[case(true, Some("0"), true, false)]
    #[case(true, None, false, false)]
    #[case(true, Some(""), false, false)]
    fn colors_require_a_supported_terminal_without_a_nonempty_no_color(
        #[case] terminal: bool,
        #[case] no_color: Option<&str>,
        #[case] supported: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            colors_allowed(terminal, no_color.map(OsStr::new), supported),
            expected
        );
    }

    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(true, true)]
    fn late_serialization_errors_leave_the_destination_untouched(
        #[case] terminal: bool,
        #[case] colors: bool,
    ) {
        let mut writer = RecordingWriter::default();
        let error = write_json(
            &mut writer,
            &LateSerializationFailure,
            terminal,
            colors,
            DEFAULT_CONTEXT,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "failed to serialize JSON output");
        assert!(format!("{error:#}").contains("late serialization failure"));
        assert!(writer.bytes.is_empty());
        assert!(!writer.flushed);
    }

    #[rstest]
    #[case(false, false)]
    #[case(true, false)]
    #[case(true, true)]
    fn successful_output_adds_one_newline_and_flushes(
        #[case] terminal: bool,
        #[case] colors: bool,
    ) {
        let value = serde_json::json!({"key": "value"});
        let mut writer = RecordingWriter::default();
        write_json(&mut writer, &value, terminal, colors, DEFAULT_CONTEXT).unwrap();
        let mut expected = format_json(&value, terminal, colors).unwrap();
        expected.push(b'\n');
        assert_eq!(writer.bytes, expected);
        assert!(writer.flushed);
    }

    #[rstest]
    #[case(true, "write context", "write failed")]
    #[case(false, "flush context", "flush failed")]
    fn output_failures_preserve_the_stage_context_and_io_cause(
        #[case] fail_write: bool,
        #[case] context: &str,
        #[case] cause: &str,
    ) {
        let mut writer = RecordingWriter {
            fail_write,
            fail_flush: !fail_write,
            ..Default::default()
        };
        let error = write_json(
            &mut writer,
            &serde_json::json!({}),
            false,
            false,
            JsonErrorContext {
                serialize: "serialize context",
                write: "write context",
                flush: "flush context",
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), context);
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().to_string(),
            cause
        );
        assert_eq!(writer.flushed, !fail_write);
    }

    fn formatted(value: &impl serde::Serialize, terminal: bool, colors: bool) -> String {
        String::from_utf8(format_json(value, terminal, colors).unwrap()).unwrap()
    }

    #[test]
    fn json_layout_and_color_follow_the_output_destination() {
        let value = serde_json::json!({"key": "value", "count": 1});
        let compact = formatted(&value, false, true);
        assert!(!compact.contains('\n'));
        assert!(!compact.contains('\x1b'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&compact).unwrap(),
            value
        );

        let pretty = formatted(&value, true, false);
        assert!(pretty.contains('\n'));
        assert!(!pretty.contains('\x1b'));
        let colored = formatted(&value, true, true);
        assert!(colored.contains('\x1b'));
        assert_eq!(console::strip_ansi_codes(&colored), pretty);
    }

    #[test]
    fn colored_output_preserves_json_escapes_and_unicode_exactly() {
        let value = serde_json::json!({
            "escaped\"key": "Unicode: é 🦀; quotes: \"; slash: \\; pair: \\\"; newline:\n; `over lint`",
            "punctuation": "{\"key\":null}",
            "values": [true, false, null, -2, 1.25e30]
        });
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let colored = formatted(&value, true, true);
        let stripped = console::strip_ansi_codes(&colored);
        assert_eq!(stripped, pretty);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stripped).unwrap(),
            value
        );
    }
}
