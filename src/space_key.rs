#![cfg_attr(not(test), forbid(unsafe_code))]

/// 对外空间标识。后端与 Wasm 共用此校验，避免接受不同的地址格式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceKey(String);

impl SpaceKey {
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty()
            || value.len() > 32
            || !value
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()))
        {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::SpaceKey;

    #[test]
    fn only_short_english_names_with_internal_single_hyphens_are_accepted() {
        for valid in ["a", "demo", "work-notes", "a-b-c", &"a".repeat(32)] {
            assert_eq!(SpaceKey::parse(valid).unwrap().as_str(), valid);
        }
        for invalid in [
            "",
            "Demo",
            "work1",
            "工作",
            "-work",
            "work-",
            "work--notes",
            " demo",
            "demo ",
            "work/notes",
            "../demo",
            "work%2Dnotes",
            "work\n",
            &"a".repeat(33),
            "01a17b4b-e399-75c6-b39f-27ea6a94702f",
        ] {
            assert!(SpaceKey::parse(invalid).is_none(), "错误接受了 {invalid:?}");
        }
    }
}
