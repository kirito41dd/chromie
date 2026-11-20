//! CLI 和服务端共用的可移植包边界。文件字节和 YAML 的解析视图分别保存。
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::error::{Error, Result};

pub const MAX_FILES: usize = 512;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillUploadFile {
    pub path: String,
    pub object_id: Uuid,
    pub digest: String,
    pub size: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishSkill {
    pub request_id: Uuid,
    pub skill_id: Option<Uuid>,
    pub expected_revision: Option<i32>,
    pub reason: String,
    pub files: Vec<SkillUploadFile>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackSkill {
    pub request_id: Uuid,
    pub skill_id: Uuid,
    pub expected_revision: i32,
    pub target_revision: i32,
    pub reason: String,
}

pub fn digest(bytes: &[u8]) -> String {
    let mut value = String::from("sha256:");
    use std::fmt::Write;
    for byte in Sha256::digest(bytes) {
        let _ = write!(value, "{byte:02x}");
    }
    value
}

/// 显式标注常见文本与二进制模板，未知格式按原始字节交付。
pub fn media_type(path: &str) -> &'static str {
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.eq_ignore_ascii_case("LICENSE") {
        return "text/plain";
    }
    match file
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some(
            "txt" | "py" | "js" | "ts" | "sh" | "nu" | "csv" | "toml" | "yaml" | "yml" | "rs"
            | "sql" | "xml" | "html" | "css",
        ) => "text/plain",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn reserved(part: &str) -> bool {
    let base = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|p| {
        base.strip_prefix(p).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

pub fn validate_path(path: &str) -> Result<()> {
    if path.len() > 512
        || path.split('/').any(|part| {
            part.is_empty()
                || part.len() > 255
                || part == "."
                || part == ".."
                || part.eq_ignore_ascii_case(".git")
                || part.ends_with(['.', ' '])
                || reserved(part)
                || part.chars().any(|c| {
                    c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
                })
        })
    {
        return Err(Error::invalid("Skill 文件路径无法安全地在不同系统间使用"));
    }
    Ok(())
}

/// 同一目录的大小写、Unicode 规范等价别名和文件/目录冲突在落盘前被拒绝。
pub fn validate_layout<'a>(entries: impl IntoIterator<Item = (&'a str, usize)>) -> Result<()> {
    let mut seen = HashMap::<String, (String, bool)>::new();
    let mut count = 0;
    let mut total = 0usize;
    for (path, size) in entries {
        validate_path(path)?;
        total = total.saturating_add(size);
        count += 1;
        if count > MAX_FILES || total > MAX_BYTES {
            return Err(Error::invalid("Skill 最多 512 个文件且合计不超过 16 MiB"));
        }
        let mut prefix = String::new();
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let key = prefix
                .nfc()
                .flat_map(char::to_lowercase)
                .collect::<String>();
            let file = parts.peek().is_none();
            if let Some((spelling, was_file)) = seen.get(&key) {
                if spelling != &prefix || file || *was_file {
                    return Err(Error::invalid("Skill 文件路径重复或存在文件与目录冲突"));
                }
            } else {
                seen.insert(key, (prefix.clone(), file));
            }
        }
    }
    if !seen
        .get("skill.md")
        .is_some_and(|(p, f)| p == "SKILL.md" && *f)
    {
        return Err(Error::invalid("Skill 根目录缺少 SKILL.md"));
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::invalid("Skill 名称应为小写英文、数字和单连字符"));
    }
    Ok(())
}

pub fn validate_package(name: &str, files: &BTreeMap<String, Vec<u8>>) -> Result<Value> {
    validate_layout(files.iter().map(|(p, b)| (p.as_str(), b.len())))?;
    let entry = files
        .get("SKILL.md")
        .ok_or_else(|| Error::invalid("缺少 SKILL.md"))?;
    let metadata = frontmatter(entry)?;
    if metadata.get("name").and_then(Value::as_str) != Some(name) {
        return Err(Error::invalid("目录名须与 SKILL.md 的 name 相同"));
    }
    Ok(metadata)
}

fn decode_header(header: &str) -> Result<Value> {
    let options = serde_saphyr::options! {
        reject_unsupported_tags: true,
        with_snippet: false,
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_total_scalar_bytes: MAX_BYTES,
        },
    };
    serde_saphyr::from_str_with_options(header, options)
        .map_err(|_| Error::invalid("Skill 的 YAML frontmatter 无效，请检查语法、重复键和字段类型"))
}

/// 只解析首段 YAML，入口及其换行方式仍按原始字节保存、传输和计算摘要。
pub fn frontmatter(entry: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(entry).map_err(|_| Error::invalid("SKILL.md 必须使用 UTF-8"))?;
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(Error::invalid("SKILL.md 缺少 YAML frontmatter 起始分隔行"));
    }
    let mut header = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            let meta = decode_header(&header)?;
            validate_name(
                meta.get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::invalid("Skill 缺少有效的 name"))?,
            )?;
            let desc = meta
                .get("description")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("Skill 缺少 description"))?;
            if desc.trim().is_empty() || desc.chars().count() > 1024 {
                return Err(Error::invalid("Skill 说明不能为空，最多 1024 个字符"));
            }
            for field in ["license", "compatibility", "allowed-tools"] {
                if meta.get(field).is_some_and(|v| !v.is_string()) {
                    return Err(Error::invalid("Skill 可选文本字段的类型无效"));
                }
            }
            if meta["compatibility"]
                .as_str()
                .is_some_and(|v| v.chars().count() > 500)
            {
                return Err(Error::invalid("compatibility 最多 500 个字符"));
            }
            if let Some(extra) = meta.get("metadata")
                && !extra
                    .as_object()
                    .is_some_and(|obj| obj.values().all(Value::is_string))
            {
                return Err(Error::invalid("Skill metadata 只接受字符串键值"));
            }
            return Ok(meta);
        }
        header.push_str(line);
    }
    Err(Error::invalid("SKILL.md 缺少 YAML frontmatter 结束分隔行"))
}

/// 已验证的发布文件以标准 Skill 目录交付。仓库不存 POSIX 模式，scripts 中有 shebang 的文件可直接执行。
#[cfg(feature = "server")]
pub(crate) fn zip_package(name: &str, files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>> {
    use std::io::{Cursor, Write};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    archive
        .add_directory(format!("{name}/"), options.unix_permissions(0o755))
        .map_err(|_| Error::unavailable())?;
    for (path, contents) in files {
        let mode = if path.starts_with("scripts/") && contents.starts_with(b"#!") {
            0o755
        } else {
            0o644
        };
        archive
            .start_file(format!("{name}/{path}"), options.unix_permissions(mode))
            .map_err(|_| Error::unavailable())?;
        archive
            .write_all(&contents)
            .map_err(|_| Error::unavailable())?;
    }
    Ok(archive
        .finish()
        .map_err(|_| Error::unavailable())?
        .into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "server")]
    #[test]
    fn portable_zip_keeps_relative_paths_bytes_and_script_mode() {
        use std::io::{Cursor, Read};
        let entries = vec![
            (
                "SKILL.md".to_owned(),
                b"---\nname: portable\ndescription: demo\n---\n".to_vec(),
            ),
            (
                "scripts/check.sh".to_owned(),
                b"#!/bin/sh\nexit 0\n".to_vec(),
            ),
            ("assets/中文 模板.bin".to_owned(), vec![0, 255, 13, 10]),
            ("references/empty.md".to_owned(), Vec::new()),
        ];
        let bytes = zip_package("portable", entries.clone()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), entries.len() + 1);
        assert!(archive.by_name("portable/").unwrap().is_dir());
        for (path, expected) in entries {
            let mut file = archive.by_name(&format!("portable/{path}")).unwrap();
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).unwrap();
            assert_eq!(expected, contents);
            assert_eq!(
                file.unix_mode().unwrap() & 0o111 != 0,
                path == "scripts/check.sh"
            );
        }
    }
    #[test]
    fn package_paths_are_portable() {
        for path in [
            "../escape",
            "a/../../b",
            "bin\\x",
            "CON.txt",
            "a//b",
            "./SKILL.md",
            "a\0b",
        ] {
            assert!(validate_path(path).is_err());
        }
        for paths in [
            ["SKILL.md", "a/b", "A/c"],
            ["SKILL.md", "a", "a/b"],
            ["SKILL.md", "café", "cafe\u{301}"],
        ] {
            assert!(validate_layout(paths.map(|p| (p, 1))).is_err());
        }
        assert!(validate_layout([("SKILL.md", 10), ("参考/模板 #1", 11)]).is_ok());
    }
    #[test]
    fn frontmatter_preserves_standard_fields() {
        let doc=b"---\r\nname: report-check\r\ndescription: >-\r\n  check\r\n  reports\r\nmetadata:\r\n  version: '1'\r\n---\r\n# test";
        let meta = frontmatter(doc).unwrap();
        assert_eq!(meta["description"], "check reports");
        assert_eq!(meta["metadata"]["version"], "1");
        assert!(frontmatter(b"---\nname: one\nname: two\ndescription: x\n---\n").is_err());
    }
}
