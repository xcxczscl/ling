//! 局域网拉取客户端（`docs/03` §5.2）：8 条并行连接、4 MB 块校验、断点续传。

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::TransferError;
use crate::http;
use crate::identity::Identity;
use crate::protocol::{self, blob_sign_payload, BLOCK_HEADER_LEN, BLOCK_SIZE, PARALLEL};
use crate::server::{framed_len, sign_headers};

/// 单次拉取任务。
pub struct FetchTask {
    /// 对端地址 `ip:port`。
    pub addr: String,
    pub identity: Arc<Identity>,
    /// 本机设备号（签名头 X-Rm-Device）。
    pub device_id: String,
    pub content_blake3: String,
    /// 落地路径（写 `<path>.part`，完成后改名）。
    pub target_path: PathBuf,
    /// 内容总字节（来源：content.size）。
    pub total: u64,
    /// 已确认偏移（断点续传起点）。
    pub start_offset: u64,
    /// 进度回调：当前确认偏移。可为 noop。
    pub on_progress: Arc<dyn Fn(u64) + Send + Sync>,
}

/// 拉取结果。
pub struct FetchOutcome {
    /// 最终确认偏移（= total）。
    pub confirmed: u64,
    pub target_path: PathBuf,
}

/// part 文件路径。
pub fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// 执行拉取：并行取块、逐块验 BLAKE3、整文件复核后改名。
pub fn fetch(task: &FetchTask) -> Result<FetchOutcome, TransferError> {
    if task.content_blake3.len() != 64 {
        return Err(TransferError::proto("内容指纹必须是 64 位十六进制"));
    }
    if let Some(parent) = task.target_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| TransferError::io(format!("创建目标目录 {parent:?}：{e}")))?;
    }
    let part = part_path(&task.target_path);
    prepare_part(&part, task.start_offset, task.total)?;

    let remaining = task.total.saturating_sub(task.start_offset);
    if remaining > 0 {
        pull_parallel(task, &part)?;
    }

    // 整文件复核。
    let digest = file_blake3(&part)?;
    if digest != task.content_blake3 {
        return Err(TransferError::corrupt(format!(
            "整文件指纹不符：算得 {digest}，预期 {}",
            task.content_blake3
        )));
    }
    std::fs::rename(&part, &task.target_path).map_err(|e| {
        TransferError::io(format!(
            "改名 {part:?} → {:?} 失败：{e}",
            task.target_path
        ))
    })?;
    (task.on_progress)(task.total);
    Ok(FetchOutcome {
        confirmed: task.total,
        target_path: task.target_path.clone(),
    })
}

/// 准备 part 文件：不存在则创建；已有数据截断到确认偏移。
fn prepare_part(part: &Path, offset: u64, total: u64) -> Result<(), TransferError> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(part)
        .map_err(|e| TransferError::io(format!("打开 {part:?}：{e}")))?;
    file.set_len(offset)
        .map_err(|e| TransferError::io(format!("截断 {part:?} 到 {offset}：{e}")))?;
    // 预留总长，避免频繁扩容（稀疏文件，实际只写确认块）。
    let _ = file.set_len(total.max(offset));
    let _ = file.set_len(offset);
    Ok(())
}

/// 并行拉取：把 [start, total) 按块对齐切成最多 PARALLEL 段，每段一条连接。
fn pull_parallel(task: &FetchTask, part: &Path) -> Result<(), TransferError> {
    let start = task.start_offset;
    let total = task.total;
    let block = BLOCK_SIZE as u64;
    // 首块可能不对齐：第一段从 start 开始，其余段对齐到块边界。
    let span = total - start;
    let lanes = (span.div_ceil(block) as usize).min(PARALLEL).max(1);
    let lane_span = span.div_ceil(lanes as u64);

    let confirmed = Arc::new(AtomicU64::new(start));
    let lane_done = Arc::new(Mutex::new(vec![false; lanes]));
    let first_err: Arc<Mutex<Option<TransferError>>> = Arc::new(Mutex::new(None));

    let mut handles = Vec::new();
    for lane in 0..lanes {
        let lane_start = start + lane as u64 * lane_span;
        if lane_start >= total {
            break;
        }
        let lane_end = (lane_start + lane_span).min(total);
        let task: Arc<FetchTask> = Arc::new(FetchTask {
            addr: task.addr.clone(),
            identity: Arc::clone(&task.identity),
            device_id: task.device_id.clone(),
            content_blake3: task.content_blake3.clone(),
            target_path: task.target_path.clone(),
            total: task.total,
            start_offset: task.start_offset,
            on_progress: Arc::clone(&task.on_progress),
        });
        let part = part.to_path_buf();
        let confirmed = Arc::clone(&confirmed);
        let lane_done = Arc::clone(&lane_done);
        let first_err = Arc::clone(&first_err);
        let on_progress = Arc::clone(&task.on_progress);
        handles.push(
            std::thread::Builder::new()
                .name(format!("rm-fetch-lane{lane}"))
                .spawn(move || {
                    let r = pull_lane(&task, &part, lane_start, lane_end);
                    if let Err(e) = r {
                        let mut slot = first_err.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                    }
                    lane_done.lock().unwrap()[lane] = true;
                    recompute_confirmed(&confirmed, &lane_done, start, lane_span, total, &on_progress);
                })
                .map_err(|e| TransferError::io(format!("启动拉取线程失败：{e}")))?,
        );
    }
    for h in handles {
        let _ = h.join();
    }
    if let Some(e) = first_err.lock().unwrap().take() {
        return Err(e);
    }
    Ok(())
}

/// 重算连续确认前缀（lane 按顺序完成才推进）。
fn recompute_confirmed(
    confirmed: &AtomicU64,
    lane_done: &Mutex<Vec<bool>>,
    start: u64,
    lane_span: u64,
    total: u64,
    on_progress: &Arc<dyn Fn(u64) + Send + Sync>,
) {
    let done = lane_done.lock().unwrap();
    let mut prefix = start;
    for (i, d) in done.iter().enumerate() {
        if !d {
            break;
        }
        prefix = (start + (i as u64 + 1) * lane_span).min(total);
    }
    confirmed.store(prefix, Ordering::SeqCst);
    on_progress(prefix);
}

/// 单连接顺序拉取 [lane_start, lane_end) 的所有块。
fn pull_lane(
    task: &FetchTask,
    part: &Path,
    lane_start: u64,
    lane_end: u64,
) -> Result<(), TransferError> {
    let block = BLOCK_SIZE as u64;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(part)
        .map_err(|e| TransferError::io(format!("打开 {part:?}：{e}")))?;

    let mut pos = lane_start;
    while pos < lane_end {
        let len = (lane_end - pos).min(block);
        fetch_block_into(task, pos, len, &mut file)?;
        pos += len;
    }
    file.flush()
        .map_err(|e| TransferError::io(format!("flush 失败：{e}")))?;
    Ok(())
}

/// 请求一个块并写入文件对应偏移；逐块验 BLAKE3。
fn fetch_block_into(
    task: &FetchTask,
    offset: u64,
    len: u64,
    file: &mut std::fs::File,
) -> Result<(), TransferError> {
    let payload = blob_sign_payload(&task.content_blake3, offset, len, protocol::now_ts());
    let headers = sign_headers(&task.identity, &task.device_id, &payload);
    let header_refs: Vec<(&str, String)> = headers.iter().map(|(k, v)| (*k, v.clone())).collect();

    let path = format!("/blob/{}?offset={offset}&len={len}", task.content_blake3);
    let expected = framed_len(len);
    let mut parser = BlockParser::new(len);

    let n = http::http_stream(&task.addr, "GET", &path, &header_refs, expected, |chunk| {
        parser.push(chunk)
    })?;
    if n != expected {
        return Err(TransferError::unreachable(format!(
            "块响应长度不符：收到 {n}，预期 {expected}"
        )));
    }
    let (data, digest) = parser.finish()?;
    let actual = blake3::hash(&data);
    if actual.as_bytes() != &digest {
        return Err(TransferError::corrupt(format!(
            "块校验失败 offset={offset}"
        )));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| TransferError::io(format!("定位文件失败：{e}")))?;
    file.write_all(&data)
        .map_err(|e| TransferError::io(format!("写入文件失败：{e}")))?;
    Ok(())
}

/// 块帧解析器：喂入字节流，产出单块 (数据, 期望摘要)。
struct BlockParser {
    buf: Vec<u8>,
    want: u64,
}

impl BlockParser {
    fn new(want: u64) -> Self {
        Self {
            buf: Vec::with_capacity((want as usize) + BLOCK_HEADER_LEN),
            want,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<(), TransferError> {
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    fn finish(mut self) -> Result<(Vec<u8>, [u8; 32]), TransferError> {
        let need = BLOCK_HEADER_LEN + self.want as usize;
        if self.buf.len() != need {
            return Err(TransferError::corrupt(format!(
                "块帧长度不符：收到 {}，预期 {need}",
                self.buf.len()
            )));
        }
        let block_len = u32::from_le_bytes(self.buf[0..4].try_into().unwrap_or([0; 4])) as usize;
        if block_len != self.want as usize {
            return Err(TransferError::corrupt(format!(
                "块帧声明长度 {block_len} ≠ 请求 {}",
                self.want
            )));
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&self.buf[4..36]);
        let data = self.buf.split_off(BLOCK_HEADER_LEN);
        Ok((data, digest))
    }
}

/// 计算文件 BLAKE3。
pub fn file_blake3(path: &Path) -> Result<String, TransferError> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|e| TransferError::io(format!("打开 {path:?}：{e}")))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| TransferError::io(format!("读 {path:?}：{e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
