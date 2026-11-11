//! Shared terminal-aware JSON output for CLI reports.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context as _, Result};
use serde::Serialize;

/// Print neutral JSON, pretty on terminals and compact in pipes.
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
    write_json(&mut io::stdout().lock(), value, terminal, context)
}

fn write_json<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    terminal: bool,
    context: JsonErrorContext,
) -> Result<()> {
    // Finish serialization before writing so failures cannot leave a partial document.
    let json = format_json(value, terminal).context(context.serialize)?;
    writer.write_all(&json).context(context.write)?;
    writer.write_all(b"\n").context(context.write)?;
    writer.flush().context(context.flush)
}

fn format_json<T: Serialize>(value: &T, terminal: bool) -> Result<Vec<u8>> {
    if terminal {
        // JSON syntax is not an identity or severity, so it uses the default foreground.
        Ok(serde_json::to_vec_pretty(value)?)
    } else {
        // Bypass all color policy in pipes, even when the environment forces colors.
        Ok(serde_json::to_vec(value)?)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use rstest::rstest;
    use serde::ser::{Serialize, SerializeSeq, Serializer};

    use super::{DEFAULT_CONTEXT, JsonErrorContext, format_json, write_json};

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
    #[case(false)]
    #[case(true)]
    fn late_serialization_errors_leave_the_destination_untouched(#[case] terminal: bool) {
        let mut writer = RecordingWriter::default();
        let error = write_json(
            &mut writer,
            &LateSerializationFailure,
            terminal,
            DEFAULT_CONTEXT,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "failed to serialize JSON output");
        assert!(format!("{error:#}").contains("late serialization failure"));
        assert!(writer.bytes.is_empty());
        assert!(!writer.flushed);
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    fn successful_output_adds_one_newline_and_flushes(#[case] terminal: bool) {
        let value = serde_json::json!({"key": "value"});
        let mut writer = RecordingWriter::default();
        write_json(&mut writer, &value, terminal, DEFAULT_CONTEXT).unwrap();
        let mut expected = format_json(&value, terminal).unwrap();
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

    fn formatted(value: &impl serde::Serialize, terminal: bool) -> String {
        String::from_utf8(format_json(value, terminal).unwrap()).unwrap()
    }

    #[test]
    fn json_layout_follows_the_output_destination_without_adding_colors() {
        let value = serde_json::json!({"key": "value", "count": 1});
        let compact = formatted(&value, false);
        assert!(!compact.contains('\n'));
        assert!(!compact.contains('\x1b'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&compact).unwrap(),
            value
        );

        let pretty = formatted(&value, true);
        assert!(pretty.contains('\n'));
        assert!(!pretty.contains('\x1b'));
    }

    #[test]
    fn terminal_output_preserves_json_escapes_and_unicode_exactly() {
        let value = serde_json::json!({
            "escaped\"key": "Unicode: é 🦀; quotes: \"; slash: \\; pair: \\\"; newline:\n; `over lint`",
            "punctuation": "{\"key\":null}",
            "values": [true, false, null, -2, 1.25e30]
        });
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let rendered = formatted(&value, true);
        assert_eq!(rendered, pretty);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            value
        );
    }
}
