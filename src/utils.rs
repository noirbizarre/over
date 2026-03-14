use std::path::Path;

use dirs::home_dir;

// Change the alias to `Box<error::Error>`.
pub type Expect<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Shorten a path by replacing the home directory with ~
pub fn short_path(path: &str) -> String {
    let Some(home) = home_dir() else {
        return path.to_string();
    };
    let home_path = home.as_path();
    let p = Path::new(path);
    if let Ok(suffix) = p.strip_prefix(home_path) {
        if suffix.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", suffix.display())
        }
    } else {
        path.to_string()
    }
}

// Find the longuest common suffix between two strings
// Returns an empty string if no common suffix is found
#[allow(dead_code)]
pub fn longuest_common_suffix<'a>(a: &'a str, b: &'a str) -> String {
    let reversed = a
        .chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a)
        .collect::<String>();
    reversed.chars().rev().collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;

    fn home() -> String {
        let h = home_dir().unwrap();
        h.to_str().unwrap().to_string()
    }

    #[rstest]
    #[case::home_dir(&home(), "~")]
    #[case::home_dir_with_path(&format!("{}/.config", home()), "~/.config")]
    #[case::outside_home("/tmp", "/tmp")]
    fn test_short_path(#[case] path: &str, #[case] expected: &str) {
        assert_eq!(short_path(path), expected.to_string());
    }

    #[rstest]
    #[case::empty("", "", "")]
    #[case::no_common_suffix("hello", "world", "")]
    #[case::common_suffix(
        "~/.dotfiles/path/to/overlay",
        "somewhere/path/to/overlay",
        "/path/to/overlay"
    )]
    #[case::common_unicode_suffix(
        "~/.dötfiles/päth/tö/ovërlay",
        "sömewhere/päth/tö/ovërlay",
        "/päth/tö/ovërlay"
    )]
    fn test_longuest_common_suffix(#[case] a: &str, #[case] b: &str, #[case] expected: &str) {
        assert_eq!(longuest_common_suffix(a, b), expected);
    }
}
