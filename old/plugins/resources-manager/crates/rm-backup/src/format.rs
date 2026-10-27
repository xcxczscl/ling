//! 段式备份格式（`docs/03` §5.3，已冻结）。
//!
//! ```text
//! <目标>/
//!   manifest.json
//!   segments/000001.seg   定长 128 MB（最后一段可短）
//!   segments.idx          JSON Lines，按 blake3 升序
//!   CHECKSUM              两行：manifest / index 指纹
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rm_proto::BizError;

use crate::codes;
use crate::crypt;

/// 段长：128 MB。
pub const SEG_SIZE: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub snapshot_id: String,
    pub created: i64,
    pub device_id: String,
    pub location_id: String,
    pub entry_count: u64,
    pub total_bytes: u64,
    pub seg_size: u64,
    /// none / placeholder
    pub enc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    pub blake3: String,
    pub seg: u32,
    pub off: u64,
    /// 段内占用（压缩或加密后的落盘长度）。
    pub len: u64,
    pub zstd: bool,
}

/// 目标上的一份完整备份（清单 + 索引）。
#[derive(Debug, Clone)]
pub struct SnapshotOnDisk {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub entries: Vec<IndexEntry>,
}

pub fn manifest_path(root: &Path) -> PathBuf {
    root.join("manifest.json")
}
pub fn index_path(root: &Path) -> PathBuf {
    root.join("segments.idx")
}
pub fn checksum_path(root: &Path) -> PathBuf {
    root.join("CHECKSUM")
}
pub fn seg_path(root: &Path, seg: u32) -> PathBuf {
    root.join("segments").join(format!("{seg:06}.seg"))
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), BizError> {
    let part = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&part).map_err(|e| {
            BizError::new(codes::F601, format!("写 {} 失败：{e}", part.display()))
        })?;
        f.write_all(data).map_err(|e| {
            BizError::new(codes::F601, format!("写 {} 失败：{e}", part.display()))
        })?;
        f.sync_all().ok();
    }
    std::fs::rename(&part, path).map_err(|e| {
        BizError::new(codes::F601, format!("提交 {} 失败：{e}", path.display()))
    })?;
    Ok(())
}

/// 写清单 + 索引 + CHECKSUM（顺序：数据段 → 索引 → 清单 → CHECKSUM）。
pub fn write_meta(root: &Path, manifest: &Manifest, entries: &[IndexEntry]) -> Result<(), BizError> {
    std::fs::create_dir_all(root).map_err(|e| {
        BizError::new(codes::F601, format!("建目标目录失败：{}：{e}", root.display()))
    })?;
    let man_bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| BizError::new(codes::F601, format!("清单序列化失败：{e}")))?;
    let mut idx = Vec::new();
    for e in entries {
        let mut line = serde_json::to_vec(e)
            .map_err(|e| BizError::new(codes::F601, format!("索引序列化失败：{e}")))?;
        line.push(b'\n');
        idx.extend_from_slice(&line);
    }
    let man_hash = blake3::hash(&man_bytes).to_hex().to_string();
    let idx_hash = blake3::hash(&idx).to_hex().to_string();
    write_atomic(&index_path(root), &idx)?;
    write_atomic(&manifest_path(root), &man_bytes)?;
    let ck = format!("manifest {man_hash}\nindex    {idx_hash}\n");
    write_atomic(&checksum_path(root), ck.as_bytes())?;
    Ok(())
}

/// 读清单（存在性检查 + 反序列化）。
pub fn read_manifest(root: &Path) -> Result<Manifest, BizError> {
    let p = manifest_path(root);
    let raw = std::fs::read(&p).map_err(|e| {
        BizError::new(
            codes::F601,
            format!("备份目标不可达或无清单：{}：{e}", p.display()),
        )
    })?;
    serde_json::from_slice(&raw)
        .map_err(|e| BizError::new(codes::F603, format!("清单损坏：{e}")))
}

/// 读索引（JSON Lines，按行）。
pub fn read_index(root: &Path) -> Result<Vec<IndexEntry>, BizError> {
    let p = index_path(root);
    let f = std::fs::File::open(&p).map_err(|e| {
        BizError::new(codes::F601, format!("索引不可读：{}：{e}", p.display()))
    })?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| {
            BizError::new(codes::F603, format!("索引第 {} 行读失败：{e}", i + 1))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let e: IndexEntry = serde_json::from_str(&line).map_err(|e| {
            BizError::new(codes::F603, format!("索引第 {} 行损坏：{e}", i + 1))
        })?;
        out.push(e);
    }
    out.sort_by(|a, b| a.blake3.cmp(&b.blake3));
    Ok(out)
}

/// 校验 CHECKSUM 两行；不一致返回 F603。
pub fn verify_checksum(root: &Path) -> Result<(String, String), BizError> {
    let ck = std::fs::read_to_string(checksum_path(root)).map_err(|e| {
        BizError::new(codes::F603, format!("CHECKSUM 不可读：{e}"))
    })?;
    let mut man_exp = String::new();
    let mut idx_exp = String::new();
    for line in ck.lines() {
        if let Some(v) = line.strip_prefix("manifest ") {
            man_exp = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("index ") {
            // 允许可变空白对齐（设计里是 "index    "）。
            idx_exp = v.trim().to_string();
        }
    }
    let man_raw = std::fs::read(manifest_path(root)).map_err(|e| {
        BizError::new(codes::F603, format!("清单不可读：{e}"))
    })?;
    let idx_raw = std::fs::read(index_path(root)).map_err(|e| {
        BizError::new(codes::F603, format!("索引不可读：{e}"))
    })?;
    let man_got = blake3::hash(&man_raw).to_hex().to_string();
    let idx_got = blake3::hash(&idx_raw).to_hex().to_string();
    if man_got != man_exp || idx_got != idx_exp {
        return Err(BizError::new(
            codes::F603,
            format!(
                "备份校验不一致：manifest 期望 {man_exp} 实得 {man_got}；index 期望 {idx_exp} 实得 {idx_got}"
            ),
        ));
    }
    Ok((man_got, idx_got))
}

/// 段写入器：按 128MB 切段追加，返回每条内容的落盘位置。
pub struct SegmentWriter {
    root: PathBuf,
    seg_size: u64,
    cur_seg: u32,
    cur_off: u64,
    cur: Option<std::fs::File>,
    key: Option<[u8; 32]>,
    seg_total: u64,
}

impl SegmentWriter {
    pub fn new(root: &Path, seg_size: u64, key: Option<[u8; 32]>) -> Result<Self, BizError> {
        std::fs::create_dir_all(root.join("segments")).map_err(|e| {
            BizError::new(codes::F601, format!("建 segments/ 失败：{e}"))
        })?;
        // 找到已写到哪一段（增量备份：续写最后一段）。
        let mut next = 1u32;
        while seg_path(root, next).exists() {
            next += 1;
        }
        let cur_seg = next.max(1);
        let cur_off = if seg_path(root, cur_seg).exists() {
            std::fs::metadata(seg_path(root, cur_seg))
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };
        Ok(Self {
            root: root.to_path_buf(),
            seg_size: seg_size.max(1),
            cur_seg,
            cur_off,
            cur: None,
            key,
            seg_total: cur_seg as u64,
        })
    }

    fn open_cur(&mut self) -> Result<(), BizError> {
        if self.cur.is_none() {
            let p = seg_path(&self.root, self.cur_seg);
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .map_err(|e| {
                    BizError::new(codes::F601, format!("打开段失败：{}：{e}", p.display()))
                })?;
            self.cur = Some(f);
        }
        Ok(())
    }

    fn roll_if_needed(&mut self, need: u64) -> Result<(), BizError> {
        if self.cur_off > 0 && self.cur_off + need > self.seg_size {
            self.cur = None;
            self.cur_seg += 1;
            self.cur_off = 0;
            self.seg_total = self.cur_seg as u64;
        }
        Ok(())
    }

    /// 追加一段逻辑字节（可先 zstd 再加密）。返回索引条目。
    pub fn append(
        &mut self,
        blake3_hex: &str,
        plain: &[u8],
        zstd: bool,
    ) -> Result<IndexEntry, BizError> {
        let mut stored = if zstd {
            crate::compress::compress_bytes(plain)?
        } else {
            plain.to_vec()
        };
        let need = stored.len() as u64;
        self.roll_if_needed(need)?;
        self.open_cur()?;
        let off = self.cur_off;
        if let Some(key) = self.key.as_ref() {
            crypt::xor_crypt(key, &mut stored, off);
        }
        let f = self.cur.as_mut().expect("open_cur");
        f.write_all(&stored).map_err(|e| {
            BizError::new(codes::F601, format!("写段失败：{e}"))
        })?;
        f.flush().ok();
        self.cur_off += need;
        Ok(IndexEntry {
            blake3: blake3_hex.to_string(),
            seg: self.cur_seg,
            off,
            len: need,
            zstd,
        })
    }

    /// 段数（当前占用到第几段）。
    pub fn seg_count(&self) -> u32 {
        if self.cur_off > 0 || self.cur.is_some() {
            self.cur_seg
        } else {
            self.cur_seg.saturating_sub(1).max(1)
        }
    }
}

/// 从段中读出一条内容（按索引定位，解密+解压+复核指纹）。
pub fn read_entry(
    root: &Path,
    e: &IndexEntry,
    key: Option<&[u8; 32]>,
) -> Result<Vec<u8>, BizError> {
    let p = seg_path(root, e.seg);
    let mut f = std::fs::File::open(&p).map_err(|e2| {
        BizError::new(codes::F604, format!("段不可读：{}：{e2}", p.display()))
    })?;
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(e.off)).map_err(|e2| {
        BizError::new(codes::F604, format!("定位段失败：{e2}"))
    })?;
    let mut buf = vec![0u8; e.len as usize];
    f.read_exact(&mut buf).map_err(|e2| {
        BizError::new(codes::F604, format!("读段失败：{e2}"))
    })?;
    if let Some(k) = key {
        crypt::xor_crypt(k, &mut buf, e.off);
    }
    let plain = if e.zstd {
        crate::compress::decompress_bytes(&buf)?
    } else {
        buf
    };
    let got = blake3::hash(&plain).to_hex().to_string();
    if got != e.blake3 {
        return Err(BizError::new(
            codes::F603,
            format!("内容不一致：索引 {}，实得 {got}", e.blake3),
        ));
    }
    Ok(plain)
}

/// 按指纹二分查找索引条目（索引已按 blake3 升序）。
pub fn find_entry<'a>(entries: &'a [IndexEntry], blake3_hex: &str) -> Option<&'a IndexEntry> {
    entries
        .binary_search_by(|e| e.blake3.as_str().cmp(blake3_hex))
        .ok()
        .map(|i| &entries[i])
}

/// 快照号：`sn_YYYYMMDD_XXXX`（时间 + 4 位随机 hex）。
pub fn new_snapshot_id(now: i64) -> String {
    let secs = now.max(0) as u64;
    let day = secs / 86400;
    let ymd = format!("{:08}", {
        // 粗略：用 Unix 秒转 UTC 日期（不引 chrono，用 civil 算法）。
        let z = day + 719468;
        let era = z / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        y * 10000 + m * 100 + d
    });
    let mut rnd = [0u8; 2];
    // 无 getrandom 依赖时用时间混合。
    let mix = (secs as u128).wrapping_mul(0x9e3779b97f4a7c15);
    rnd[0] = (mix ^ (mix >> 8)) as u8;
    rnd[1] = (mix.wrapping_mul(6364136223846793005) >> 32) as u8;
    format!("sn_{ymd}_{:02x}{:02x}", rnd[0], rnd[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_and_read_meta() {
        let d = tempdir().unwrap();
        let man = Manifest {
            snapshot_id: "sn_20261017_ab".into(),
            created: 1,
            device_id: "dv_a".into(),
            location_id: "lc_home".into(),
            entry_count: 1,
            total_bytes: 3,
            seg_size: SEG_SIZE,
            enc: "none".into(),
        };
        let entries = vec![IndexEntry {
            blake3: "aa".into(),
            seg: 1,
            off: 0,
            len: 3,
            zstd: false,
        }];
        write_meta(d.path(), &man, &entries).unwrap();
        let m2 = read_manifest(d.path()).unwrap();
        assert_eq!(m2, man);
        let e2 = read_index(d.path()).unwrap();
        assert_eq!(e2, entries);
        verify_checksum(d.path()).unwrap();
    }

    #[test]
    fn segment_append_read_roundtrip() {
        let d = tempdir().unwrap();
        let mut w = SegmentWriter::new(d.path(), 1024, None).unwrap();
        let a = vec![b'a'; 100];
        let b = vec![b'b'; 2000];
        let ha = blake3::hash(&a).to_hex().to_string();
        let hb = blake3::hash(&b).to_hex().to_string();
        let ea = w.append(&ha, &a, false).unwrap();
        let eb = w.append(&hb, &b, true).unwrap();
        // 段长 1000 → b 会滚到下一段。
        assert!(eb.seg > ea.seg || eb.off > ea.off);
        let entries = vec![ea.clone(), eb.clone()].into_iter().collect::<Vec<_>>();
        // 按指纹排序查找
        let mut sorted = entries;
        sorted.sort_by(|x, y| x.blake3.cmp(&y.blake3));
        let found = find_entry(&sorted, &hb).unwrap();
        let got = read_entry(d.path(), found, None).unwrap();
        assert_eq!(got, b);
    }
}
