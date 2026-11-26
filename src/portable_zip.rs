//! ZIP32 stored/deflate transport for offline libraries (no credentials, no executable entries).
//! Container specification: https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT
use anyhow::{Context, Result};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
fn u16le(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes([b[n], b[n + 1]])
}
fn u32le(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn w16(w: &mut impl Write, n: u16) -> Result<()> {
    w.write_all(&n.to_le_bytes())?;
    Ok(())
}
fn w32(w: &mut impl Write, n: u32) -> Result<()> {
    w.write_all(&n.to_le_bytes())?;
    Ok(())
}
fn zip32(n: u64) -> Result<u32> {
    anyhow::ensure!(
        n < u32::MAX as u64,
        "资料包超出 ZIP32 容量；请按账号或目录分别导出"
    );
    Ok(n as u32)
}
fn reserved(part: &str) -> bool {
    let name = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    ["CON", "PRN", "AUX", "NUL"].contains(&name.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            name.strip_prefix(p)
                .is_some_and(|s| s.len() == 1 && matches!(s.as_bytes()[0], b'1'..=b'9'))
        })
}
pub fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && !Path::new(name).is_absolute()
        && name.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && !reserved(p)
                && !p.ends_with(['.', ' '])
                && !p.chars().any(char::is_control)
        })
}
fn allowed(name: &str) -> bool {
    safe_name(name)
        && ["archive-v1/", "checkpoints/", "ng/"]
            .iter()
            .any(|p| name.starts_with(p))
}
pub fn create(dest: &Path, files: &[(String, PathBuf)]) -> Result<()> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, path) in files {
        anyhow::ensure!(
            allowed(name) && seen.insert(name.to_lowercase()),
            "资料包文件名无效或重复"
        );
        let bytes = name.as_bytes();
        let len: u16 = bytes.len().try_into()?;
        let offset = zip32(temp.stream_position()?)?;
        w32(&mut temp, 0x04034b50)?;
        w16(&mut temp, 20)?;
        w16(&mut temp, 0x808)?;
        w16(&mut temp, 0)?;
        w16(&mut temp, 0)?;
        w16(&mut temp, 33)?;
        for _ in 0..3 {
            w32(&mut temp, 0)?;
        }
        w16(&mut temp, len)?;
        w16(&mut temp, 0)?;
        temp.write_all(bytes)?;
        let mut f = File::open(path)?;
        let mut crc = crc32fast::Hasher::new();
        let mut size = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            zip32(size)?;
            crc.update(&buf[..n]);
            temp.write_all(&buf[..n])?;
        }
        let size = zip32(size)?;
        let crc = crc.finalize();
        w32(&mut temp, 0x08074b50)?;
        w32(&mut temp, crc)?;
        w32(&mut temp, size)?;
        w32(&mut temp, size)?;
        entries.push((name.clone(), crc, size, offset));
    }
    let start = zip32(temp.stream_position()?)?;
    let count: u16 = entries
        .len()
        .try_into()
        .context("资料包文件过多，请分批导出")?;
    anyhow::ensure!(count < u16::MAX, "资料包文件过多，请分批导出");
    for (name, crc, size, offset) in entries {
        w32(&mut temp, 0x02014b50)?;
        w16(&mut temp, 20)?;
        w16(&mut temp, 20)?;
        w16(&mut temp, 0x808)?;
        w16(&mut temp, 0)?;
        w16(&mut temp, 0)?;
        w16(&mut temp, 33)?;
        w32(&mut temp, crc)?;
        w32(&mut temp, size)?;
        w32(&mut temp, size)?;
        w16(&mut temp, name.len() as u16)?;
        for _ in 0..4 {
            w16(&mut temp, 0)?;
        }
        w32(&mut temp, 0)?;
        w32(&mut temp, offset)?;
        temp.write_all(name.as_bytes())?;
    }
    let length = zip32(temp.stream_position()? - (start as u64))?;
    w32(&mut temp, 0x06054b50)?;
    w16(&mut temp, 0)?;
    w16(&mut temp, 0)?;
    w16(&mut temp, count)?;
    w16(&mut temp, count)?;
    w32(&mut temp, length)?;
    w32(&mut temp, start)?;
    w16(&mut temp, 0)?;
    temp.as_file().sync_all()?;
    temp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}
#[cfg(test)]
fn extract(source: &Path, dest: &Path) -> Result<()> {
    extract_with_workers(source, dest, crate::local_read::workers())
}

pub fn extract_with_workers(source: &Path, dest: &Path, workers: usize) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let base = dest.canonicalize()?;
    let mut file = File::open(source)?;
    let length = file.metadata()?.len();
    let tail_len = length.min(65557) as usize;
    anyhow::ensure!(tail_len >= 22, "资料包不是有效 ZIP");
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)?;
    let e = (0..=tail_len - 22)
        .rev()
        .find(|i| {
            u32le(&tail, *i) == 0x06054b50 && *i + 22 + u16le(&tail, *i + 20) as usize == tail_len
        })
        .context("缺少 ZIP 目录")?;
    anyhow::ensure!(
        u16le(&tail, e + 4) == 0
            && u16le(&tail, e + 6) == 0
            && u16le(&tail, e + 8) == u16le(&tail, e + 10),
        "不支持分卷资料包"
    );
    let count = u16le(&tail, e + 10);
    let central = u32le(&tail, e + 16) as u64;
    let central_len = u32le(&tail, e + 12) as u64;
    anyhow::ensure!(
        count != u16::MAX && central != u32::MAX as u64 && central_len != u32::MAX as u64,
        "暂不支持 ZIP64，请分批导出"
    );
    anyhow::ensure!(
        central + central_len <= length - tail_len as u64 + e as u64,
        "ZIP 目录越界"
    );
    file.seek(SeekFrom::Start(central))?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let mut h = [0u8; 46];
        file.read_exact(&mut h)?;
        anyhow::ensure!(u32le(&h, 0) == 0x02014b50, "ZIP 目录损坏");
        let flags = u16le(&h, 8);
        let method = u16le(&h, 10);
        anyhow::ensure!(
            flags & 0x41 == 0 && [0, 8].contains(&method),
            "资料包加密或压缩方式不支持"
        );
        anyhow::ensure!(
            u16le(&h, 34) == 0 && (u32le(&h, 38) >> 16) & 0xf000 != 0xa000,
            "资料包含分卷或符号链接"
        );
        let mut name = vec![0; u16le(&h, 28) as usize];
        file.read_exact(&mut name)?;
        let name = String::from_utf8(name).context("资料包文件名编码不受支持")?;
        file.seek(SeekFrom::Current(
            (u16le(&h, 30) as i64) + (u16le(&h, 32) as i64),
        ))?;
        let directory = name.ends_with('/');
        let key = name.trim_end_matches('/');
        anyhow::ensure!(
            safe_name(key)
                && (allowed(key)
                    || directory && ["archive-v1", "checkpoints", "ng"].contains(&key)),
            "资料包包含非法路径"
        );
        // Directory entries can be omitted; extracted files create their own parents.
        if directory {
            anyhow::ensure!(safe_name(key), "非法目录");
            continue;
        }
        anyhow::ensure!(seen.insert(name.to_lowercase()), "资料包存在重复文件名");
        let size = u32le(&h, 24);
        let packed = u32le(&h, 20);
        let offset = u32le(&h, 42);
        anyhow::ensure!(
            size != u32::MAX && packed != u32::MAX && offset != u32::MAX,
            "暂不支持 ZIP64"
        );
        entries.push((name, flags, method, u32le(&h, 16), size, packed, offset));
    }
    anyhow::ensure!(
        file.stream_position()? == central + central_len,
        "ZIP 目录长度不符"
    );
    entries.sort_by_key(|entry| entry.6);
    // Validate the topology before any workers start creating directories.
    for (name, ..) in &entries {
        for parent in Path::new(name).ancestors().skip(1) {
            anyhow::ensure!(
                !seen.contains(&parent.to_string_lossy().replace('\\', "/").to_lowercase()),
                "ZIP 文件与目录同名"
            );
        }
    }
    let (stored, compressed): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(|entry| entry.2 == 0);
    extract_entries(&mut file, &base, central, &stored)?;
    if compressed.is_empty() {
        return Ok(());
    }
    if workers <= 1 || compressed.len() < 2 {
        return extract_entries(&mut file, &base, central, &compressed);
    }
    let chunk_size = compressed.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let jobs: Vec<_> = compressed
            .chunks(chunk_size)
            .map(|entries| {
                let base = &base;
                scope.spawn(move || {
                    // Each worker needs its own open file description and cursor.
                    let mut file = File::open(source)?;
                    extract_entries(&mut file, base, central, entries)
                })
            })
            .collect();
        let mut result = Ok(());
        for job in jobs {
            let item = job.join().expect("ZIP worker panicked");
            if result.is_ok() {
                result = item;
            }
        }
        result
    })
}

type Entry = (String, u16, u16, u32, u32, u32, u32);

fn extract_entries(file: &mut File, base: &Path, central: u64, entries: &[Entry]) -> Result<()> {
    for (name, flags, method, expected_crc, size, packed, offset) in entries {
        let (flags, method, expected_crc, size, packed, offset) =
            (*flags, *method, *expected_crc, *size, *packed, *offset);
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut h = [0u8; 30];
        file.read_exact(&mut h)?;
        anyhow::ensure!(
            u32le(&h, 0) == 0x04034b50 && u16le(&h, 6) == flags && u16le(&h, 8) == method,
            "ZIP 文件头不匹配"
        );
        let mut local_name = vec![0; u16le(&h, 26) as usize];
        file.read_exact(&mut local_name)?;
        anyhow::ensure!(local_name == name.as_bytes(), "ZIP 本地文件名不匹配");
        file.seek(SeekFrom::Current(u16le(&h, 28) as i64))?;
        anyhow::ensure!(
            file.stream_position()? + packed as u64 <= central,
            "ZIP 文件数据越界"
        );
        let path = base.join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        anyhow::ensure!(
            path.parent().unwrap().canonicalize()?.starts_with(base),
            "解压路径越界"
        );
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let input = Read::by_ref(file).take(packed as u64);
        let mut input: Box<dyn Read + '_> = if method == 8 {
            Box::new(flate2::read::DeflateDecoder::new(input))
        } else {
            Box::new(input)
        };
        let mut crc = crc32fast::Hasher::new();
        let mut total = 0u64;
        let mut buf = [0u8; 65536];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            anyhow::ensure!(total <= size as u64, "ZIP 展开长度超出声明");
            crc.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        anyhow::ensure!(
            total == size as u64 && crc.finalize() == expected_crc,
            "资料包文件 CRC 或长度校验失败"
        );
        out.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_deflate_archive_matches_between_serial_and_parallel_extraction() {
        // Generated with Python's ZIP_DEFLATED writer, with three separate entries.
        let encoded = concat!(
            "504b0304140000000800782e7a5d3920dfd5110000000008000011000000617263686976652d76312f302e6a736f6e73741c05a3",
            "60148c8251300a46c1480300504b0304140000000800782e7a5dac6d286b110000000008000011000000617263686976652d7631",
            "2f312e6a736f6e73721a05a360148c8251300a46c1480300504b0304140000000800782e7a5de054aab711000000000800001100",
            "0000617263686976652d76312f322e6a736f6e73761e05a360148c8251300a46c1480300504b01021400140000000800782e7a5d",
            "3920dfd51100000000080000110000000000000000000000800100000000617263686976652d76312f302e6a736f6e504b010214",
            "00140000000800782e7a5dac6d286b1100000000080000110000000000000000000000800140000000617263686976652d76312f",
            "312e6a736f6e504b01021400140000000800782e7a5de054aab71100000000080000110000000000000000000000800180000000",
            "617263686976652d76312f322e6a736f6e504b05060000000003000300bd000000c00000000000",
        );
        let bytes: Vec<u8> = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let temp = tempfile::tempdir().unwrap();
        let zip = temp.path().join("external.zip");
        std::fs::write(&zip, &bytes).unwrap();
        for workers in [1, 4] {
            let output = temp.path().join(format!("workers-{workers}"));
            extract_with_workers(&zip, &output, workers).unwrap();
            for index in 0..3u8 {
                assert_eq!(
                    std::fs::read(output.join(format!("archive-v1/{index}.json"))).unwrap(),
                    vec![b'A' + index; 2048],
                );
            }
        }
        let mut damaged = bytes;
        damaged[52] ^= 0x3f;
        std::fs::write(&zip, damaged).unwrap();
        assert!(extract_with_workers(&zip, &temp.path().join("damaged"), 4).is_err());
    }

    #[test]
    #[ignore = "set BILIARCHIVE_BENCH_ZIP to a large archive and run with --ignored --nocapture"]
    fn benchmark_external_zip() {
        let input = PathBuf::from(std::env::var_os("BILIARCHIVE_BENCH_ZIP").unwrap());
        let stage = tempfile::tempdir().unwrap();
        for workers in [1, crate::local_read::workers()] {
            let output = stage.path().join(format!("workers-{workers}"));
            let now = std::time::Instant::now();
            extract_with_workers(&input, &output, workers).unwrap();
            eprintln!(
                "{workers} workers: {:?} to extract {} bytes",
                now.elapsed(),
                input.metadata().unwrap().len()
            );
            std::fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn stored_roundtrip_and_path_rejection() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("file");
        std::fs::write(&f, b"archive bytes").unwrap();
        let zip = d.path().join("bundle.zip");
        create(&zip, &[("archive-v1/.library.json".into(), f)]).unwrap();
        let out = d.path().join("out");
        extract(&zip, &out).unwrap();
        assert_eq!(
            std::fs::read(out.join("archive-v1/.library.json")).unwrap(),
            b"archive bytes"
        );
        for p in ["../x", "/x", "C:/x", "a/../b", "a\\b", "a:ads", "a./b"] {
            assert!(!safe_name(p));
        }
    }
}

#[cfg(test)]
mod corruption_tests {
    use super::*;
    #[test]
    fn corrupt_payload_and_duplicate_names_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("file");
        std::fs::write(&f, b"payload").unwrap();
        let zip = d.path().join("data.zip");
        let name = "archive-v1/data.json";
        create(&zip, &[(name.into(), f.clone())]).unwrap();
        let mut bytes = std::fs::read(&zip).unwrap();
        bytes[30 + name.len()] ^= 1;
        std::fs::write(&zip, bytes).unwrap();
        assert!(extract(&zip, &d.path().join("out")).is_err());
        assert!(create(&zip, &[(name.into(), f.clone()), (name.to_uppercase(), f)]).is_err());
    }
}
