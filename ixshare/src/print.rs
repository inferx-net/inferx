// Copyright (c) 2025 InferX Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::string::String;
use alloc::vec::Vec;
use chrono::prelude::*;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use lazy_static::lazy_static;
use spin::RwLock;
use std::arch::asm;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

lazy_static! {
    pub static ref LOG: Log = Log::New();
    pub static ref START_TIME: i64 = Utc::now().timestamp_millis();
    pub static ref LOG_MONITOR: LogMonitor = LogMonitor::New();
}

/// Global switch for trace-level logging; used by the `trace!` macro.
pub static TRACE_LOG_ENABLED: AtomicBool = AtomicBool::new(false);
pub static VERBOSE_LOG_MASK: AtomicU64 = AtomicU64::new(0);
pub const INFERX_VERBOSE_LOG_ENV: &str = "INFERX_VERBOSE_LOG";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VerboseCategory {
    pub mask: u64,
    pub name: &'static str,
    pub env_name: &'static str,
}

pub mod verbose_category {
    use super::VerboseCategory;

    pub const MCP: VerboseCategory = VerboseCategory {
        mask: 1 << 0,
        name: "MCP",
        env_name: "mcp",
    };
    pub const SCHEDULER: VerboseCategory = VerboseCategory {
        mask: 1 << 1,
        name: "SCHEDULER",
        env_name: "scheduler",
    };
    pub const FUNC_AGENT: VerboseCategory = VerboseCategory {
        mask: 1 << 2,
        name: "FUNC_AGENT",
        env_name: "func_agent",
    };
    pub const ROUTING: VerboseCategory = VerboseCategory {
        mask: 1 << 3,
        name: "ROUTING",
        env_name: "routing",
    };
    pub const SKILL: VerboseCategory = VerboseCategory {
        mask: 1 << 4,
        name: "SKILL",
        env_name: "skill",
    };
    pub const SKILL_PAYLOAD: VerboseCategory = VerboseCategory {
        mask: 1 << 5,
        name: "SKILL_PAYLOAD",
        env_name: "skill_payload",
    };
    pub const AGENT: VerboseCategory = VerboseCategory {
        mask: 1 << 6,
        name: "AGENT",
        env_name: "agent",
    };

    pub const ALL: &[VerboseCategory] =
        &[MCP, SCHEDULER, FUNC_AGENT, ROUTING, SKILL, SKILL_PAYLOAD, AGENT];
}

#[inline]
pub fn trace_logging_enabled() -> bool {
    TRACE_LOG_ENABLED.load(Ordering::Relaxed)
}

#[inline]
pub fn set_trace_logging(enable: bool) {
    TRACE_LOG_ENABLED.store(enable, Ordering::SeqCst);
}

#[inline]
pub fn verbose_category_enabled(category: VerboseCategory) -> bool {
    VERBOSE_LOG_MASK.load(Ordering::Relaxed) & category.mask != 0
}

#[inline]
pub fn verbose_log_mask() -> u64 {
    VERBOSE_LOG_MASK.load(Ordering::Relaxed)
}

#[inline]
pub fn set_verbose_log_mask(mask: u64) {
    VERBOSE_LOG_MASK.store(mask, Ordering::SeqCst);
}

#[inline]
pub fn replace_verbose_log_mask(mask: u64) -> u64 {
    VERBOSE_LOG_MASK.swap(mask, Ordering::SeqCst)
}

#[inline]
pub fn enable_verbose_category(category: VerboseCategory) -> u64 {
    VERBOSE_LOG_MASK.fetch_or(category.mask, Ordering::SeqCst)
}

#[inline]
pub fn disable_verbose_category(category: VerboseCategory) -> u64 {
    VERBOSE_LOG_MASK.fetch_and(!category.mask, Ordering::SeqCst)
}

pub fn parse_verbose_log_mask(input: &str) -> u64 {
    parse_verbose_log_mask_internal(input).0
}

pub fn parse_verbose_log_mask_with_unknowns(input: &str) -> (u64, Vec<String>) {
    parse_verbose_log_mask_internal(input)
}

pub fn mask_to_env_names(mask: u64) -> Vec<&'static str> {
    verbose_category::ALL
        .iter()
        .filter(|cat| mask & cat.mask != 0)
        .map(|cat| cat.env_name)
        .collect()
}

pub fn verbose_category_by_env_name(input: &str) -> Option<VerboseCategory> {
    let normalized = input.trim().to_ascii_lowercase();
    verbose_category::ALL
        .iter()
        .copied()
        .find(|cat| cat.env_name == normalized)
}

pub fn init_verbose_logging_from_env() -> u64 {
    let raw = std::env::var(INFERX_VERBOSE_LOG_ENV).unwrap_or_default();
    let (mask, unknowns) = parse_verbose_log_mask_with_unknowns(&raw);
    set_verbose_log_mask(mask);

    for unknown in unknowns {
        crate::warn!(
            "unknown verbose category in {}: {}",
            INFERX_VERBOSE_LOG_ENV,
            unknown
        );
    }

    let enabled = mask_to_env_names(mask);
    crate::info!(
        "verbose categories initialized from {} raw='{}' categories={:?} mask={}",
        INFERX_VERBOSE_LOG_ENV,
        raw,
        enabled,
        mask
    );
    mask
}

fn parse_verbose_log_mask_internal(input: &str) -> (u64, Vec<String>) {
    let mut mask = 0u64;
    let mut unknowns = Vec::new();

    for token in input.split(',') {
        let normalized = token.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            continue;
        }

        match verbose_category::ALL
            .iter()
            .find(|cat| cat.env_name == normalized)
            .copied()
        {
            Some(category) => mask |= category.mask,
            None => {
                if !unknowns.iter().any(|value| value == &normalized) {
                    unknowns.push(normalized);
                }
            }
        }
    }

    (mask, unknowns)
}

#[inline(always)]
#[cfg(target_arch = "x86_64")]
pub fn RawRdtsc() -> i64 {
    let rax: u64;
    let rdx: u64;
    unsafe {
        asm!("
            lfence
            rdtsc
            ",
            out("rax") rax,
            out("rdx") rdx
        )
    };

    return rax as i64 | ((rdx as i64) << 32);
}

pub fn ThreadId() -> i32 {
    unsafe {
        return libc::gettid();
    }
    // return -1;
}

pub struct Log {
    pub fd: AtomicI32,
    pub rawfd: AtomicI32,
    pub lineNum: AtomicU64,
    pub syncPrint: AtomicBool,
    pub processid: AtomicI32,
    pub serviceName: RwLock<String>,
    pub fileStart: AtomicU64,
}

pub fn SetSyncPrint(syncPrint: bool) {
    LOG.SetSyncPrint(syncPrint);
}

pub const LOG_FILE_DEFAULT: &str = "/opt/inferx/log/onenode.log";
pub const RAWLOG_FILE_DEFAULT: &str = "/opt/inferx/log/raw.log";
pub const LOG_FILE_FORMAT: &str = "/opt/inferx/log/{}.log";
pub const TIME_FORMAT: &str = "%H:%M:%S%.3f";

pub const LOG_ROTATION_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

pub const MEMORY_LEAK_LOG: bool = false;

pub struct LogMonitor {
    pub watcher: notify::RecommendedWatcher,
    pub dummy: i32,
}

impl LogMonitor {
    pub fn New() -> Self {
        use notify::Watcher;

        println!("Log start to watch ... start monitor");
        let log_path = std::path::PathBuf::from(LOG_FILE_DEFAULT);
        let log_dir = log_path.parent().unwrap();
        // start inotify watcher
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let reopen = match &event.kind {
                        notify::EventKind::Remove(_) => event
                            .paths
                            .iter()
                            .any(|p| p == std::path::Path::new(LOG_FILE_DEFAULT)),
                        _ => false,
                    };

                    if reopen {
                        println!("Log Monitor get log removal event {:?}", &event);
                        let file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(LOG_FILE_DEFAULT)
                            .expect("Log Open fail");
                        let newfd = file.into_raw_fd();
                        LOG.ResetFd(newfd);
                    }
                }
                Err(e) => eprintln!("watch error: {:?}", e),
            })
            .unwrap();

        watcher
            .watch(&log_dir, notify::RecursiveMode::NonRecursive)
            .unwrap();

        return Self {
            watcher: watcher,
            dummy: 0,
        };
    }

    pub fn Touch(&self) -> i32 {
        return self.dummy;
    }
}

#[inline]
pub fn Timestamp() -> i64 {
    // let tsc = RawRdtsc();
    // (tsc as i128 / *CPU_FREQ as i128) as i64

    let now = Utc::now();
    return now.timestamp_millis() - *START_TIME;
}

impl Drop for Log {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.Logfd());
        }
    }
}

impl Log {
    pub fn New() -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_FILE_DEFAULT)
            .expect("Log Open fail");

        let rawfile = OpenOptions::new()
            .create(true)
            .append(true)
            .open(RAWLOG_FILE_DEFAULT)
            .expect("Log Open fail");

        println!("Log start to watch ...");
        // Log::Monitor();
        LOG_MONITOR.Touch();

        let start_secs = Self::now_secs();

        return Self {
            fd: AtomicI32::new(file.into_raw_fd()),
            rawfd: AtomicI32::new(rawfile.into_raw_fd()),
            lineNum: AtomicU64::new(1),
            syncPrint: AtomicBool::new(true),
            processid: AtomicI32::new(std::process::id() as _),
            serviceName: RwLock::new("".to_owned()),
            fileStart: AtomicU64::new(start_secs),
        };
    }

    pub fn ResetFd(&self, newfd: i32) {
        let oldfd = self.Logfd();
        self.fd.store(newfd, Ordering::SeqCst);
        unsafe {
            libc::close(oldfd);
        }
    }

    pub fn SetServiceName(&self, name: &str) {
        *self.serviceName.write() = name.to_owned();
    }

    fn reopen_default_log(&self) -> std::io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(LOG_FILE_DEFAULT)?;
        self.ResetFd(file.into_raw_fd());
        self.set_file_start_now();
        Ok(())
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs()
    }

    fn set_file_start_now(&self) {
        self.fileStart.store(Self::now_secs(), Ordering::SeqCst);
    }

    fn file_age(&self) -> Duration {
        let start = self.fileStart.load(Ordering::Relaxed);
        Duration::from_secs(Self::now_secs().saturating_sub(start))
    }

    fn rotation_target_path() -> Option<PathBuf> {
        let log_path = Path::new(LOG_FILE_DEFAULT);
        let dir = log_path.parent()?;
        let base = log_path.file_name()?.to_string_lossy();
        let suffix = Local::now().format("%Y%m%d%H%M%S").to_string();
        Some(dir.join(format!("{}.{}", base, suffix)))
    }

    /// Returns the age of the log file based on its creation time.
    /// This is used for cross-process coordination.
    fn actual_file_age() -> Duration {
        match std::fs::metadata(LOG_FILE_DEFAULT) {
            Ok(meta) => {
                // Use creation time (birth time) if available, fall back to modified time
                let file_time = meta.created().or_else(|_| meta.modified());
                match file_time {
                    Ok(time) => SystemTime::now()
                        .duration_since(time)
                        .unwrap_or(Duration::ZERO),
                    Err(_) => Duration::ZERO,
                }
            }
            Err(_) => Duration::ZERO, // File doesn't exist or can't be read
        }
    }

    fn rotate_if_needed(&self, retention: Duration) {
        if retention.is_zero() {
            return;
        }

        // Quick check using in-memory timestamp (avoids syscall most of the time)
        if self.file_age() < retention {
            return;
        }

        // Try to acquire exclusive lock for rotation (cross-process synchronization)
        let lock_path = Path::new(LOG_FILE_DEFAULT).with_extension("rotation.lock");
        let lock_file = match std::fs::File::create(&lock_path) {
            Ok(f) => f,
            Err(_) => return,
        };

        // Non-blocking exclusive lock - if another process is rotating, skip
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            return;
        }

        // Re-check using actual file metadata after acquiring lock
        // Another process may have just completed rotation
        if Self::actual_file_age() < retention {
            // File was recently rotated by another process, sync our local state
            self.set_file_start_now();
            return; // Lock released when lock_file is dropped
        }

        if let Some(target) = Self::rotation_target_path() {
            match std::fs::rename(LOG_FILE_DEFAULT, &target) {
                Ok(_) => {
                    if let Err(err) = self.reopen_default_log() {
                        eprintln!("Failed to reopen log after rotation: {:?}", err);
                    }
                    println!(
                        "Rotated {} to {:?} after keeping {:?}",
                        LOG_FILE_DEFAULT, target, retention
                    );
                    self.delete_old_rotated_logs(retention);
                }
                Err(err) => match err.kind() {
                    ErrorKind::NotFound => {
                        // Another process already rotated, just reopen
                        if let Err(err) = self.reopen_default_log() {
                            eprintln!("Failed to reopen log after missing path: {:?}", err);
                        }
                    }
                    _ => {
                        eprintln!("Failed to rotate log file: {:?}", err);
                    }
                },
            }
        } else {
            eprintln!("Failed to determine rotated log path");
        }
        // Lock automatically released when lock_file is dropped
    }

    fn delete_old_rotated_logs(&self, retention: Duration) {
        if retention.is_zero() {
            return;
        }

        let log_path = Path::new(LOG_FILE_DEFAULT);
        let dir = match log_path.parent() {
            Some(parent) => parent,
            None => return,
        };

        let base_name = match log_path.file_name().and_then(OsStr::to_str) {
            Some(name) => name.to_owned(),
            None => return,
        };

        let prefix = format!("{}.", base_name);
        let now = SystemTime::now();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }

                    let file_name = match path.file_name().and_then(OsStr::to_str) {
                        Some(name) => name,
                        None => continue,
                    };

                    if !file_name.starts_with(&prefix) {
                        continue;
                    }

                    if let Ok(metadata) = entry.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if now.duration_since(modified).unwrap_or_default() > retention {
                                if let Err(err) = std::fs::remove_file(&path) {
                                    eprintln!("Failed to delete rotated log {:?}: {:?}", path, err);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn ServiceName(&self) -> String {
        return self.serviceName.read().clone();
    }

    pub fn Reset(&self, name: &str) {
        let filename = format!("/opt/inferx/log/{}.log", name);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(filename)
            .expect("Log Open fail");

        unsafe {
            libc::close(self.Logfd());
        }

        self.fd.store(file.into_raw_fd(), Ordering::SeqCst);
        self.set_file_start_now();
    }

    pub fn Logfd(&self) -> i32 {
        return self.fd.load(Ordering::Relaxed);
    }

    pub fn SyncPrint(&self) -> bool {
        return self.syncPrint.load(Ordering::Relaxed);
    }

    pub fn SetSyncPrint(&self, syncPrint: bool) {
        self.syncPrint.store(syncPrint, Ordering::SeqCst);
    }

    pub fn RawWrite(&self, str: &str) {
        let str = if MEMORY_LEAK_LOG {
            format!("{:?} {:?} {}", self.processid, self.lineNum, str)
        } else {
            format!("{}", str)
        };
        self.WriteAll(str.as_bytes());
    }

    pub fn Write(&self, str: &str) {
        self.WriteAll(str.as_bytes());
    }

    fn write(&self, buf: &[u8]) -> i32 {
        let ret = unsafe {
            libc::write(
                self.Logfd(),
                &buf[0] as *const _ as u64 as *const _,
                buf.len() as _,
            )
        };

        if ret < 0 {
            panic!("log write fail ...")
        }

        return ret as i32;
    }

    pub fn WriteAll(&self, buf: &[u8]) {
        // self.rotate_if_needed(LOG_ROTATION_RETENTION);
        self.lineNum.fetch_add(1, Ordering::Relaxed);
        let mut count = 0;
        while count < buf.len() {
            let n = self.write(&buf[count..]);
            count += n as usize;
        }
    }

    pub fn RawLog(&self, val1: u64, val2: u64, val3: u64, val4: u64) {
        let data = [
            self.processid.load(Ordering::Relaxed) as u64,
            self.lineNum.load(Ordering::Relaxed),
            val1,
            val2,
            val3,
            val4,
        ];

        let addr = &data[0] as *const _ as u64;
        let mut count = 0;

        while count < 8 * data.len() {
            let n = unsafe {
                libc::write(
                    self.rawfd.load(Ordering::Relaxed),
                    (addr + count as u64) as *const _,
                    8 * data.len() - count as usize,
                )
            };

            if n < 0 {
                panic!("log write fail ...")
            }

            count += n as usize;
        }
    }

    pub fn Now() -> String {
        return Local::now().format(TIME_FORMAT).to_string();
    }

    pub fn Print(&self, level: &str, str: &str) {
        // let now = Timestamp();

        let now = Local::now();
        let formatted = now.format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        if MEMORY_LEAK_LOG {
            self.Write(&format!(
                "{:?} [{}] [{}/{}] {}\n",
                self.processid,
                level,
                ThreadId(),
                formatted,
                str
            ));
        } else {
            self.Write(&format!(
                "{}[{}] [{}/{}] {}\n",
                self.ServiceName(),
                level,
                ThreadId(),
                formatted,
                str
            ));
        }
    }

    pub fn RawPrint(&self, level: &str, str: &str) {
        self.RawWrite(&format!("[{}] {}\n", level, str));
    }
}

#[macro_export]
macro_rules! raw {
    // macth like arm for macro
    ($a:expr,$b:expr,$c:expr,$d:expr) => {{
        $crate::print::LOG.RawLog($a, $b, $c, $d);
    }};
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.RawWrite(&format!("{}\n",&s));
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.RawPrint("Print", &s);
    });
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.Print("ERROR", &s);
    });
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.Print("INFO", &s);
    });
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.Print("WARN", &s);
    });
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => ({
        let s = &format!($($arg)*);
        $crate::print::LOG.Print("DEBUG", &s);
    });
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => ({
        if $crate::print::trace_logging_enabled() {
            let s = &format!($($arg)*);
            $crate::print::LOG.Print("TRACE", &s);
        }
    });
}

#[macro_export]
macro_rules! ctrace {
    ($category:expr, $($arg:tt)*) => ({
        let category = $category;
        if $crate::print::verbose_category_enabled(category) {
            let s = &format!($($arg)*);
            $crate::print::LOG.Print("CTRACE", &format!("[{}] {}", category.name, s));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        disable_verbose_category, enable_verbose_category, mask_to_env_names,
        parse_verbose_log_mask, parse_verbose_log_mask_with_unknowns, replace_verbose_log_mask,
        set_verbose_log_mask, verbose_category, verbose_category_by_env_name, verbose_log_mask,
    };
    use std::collections::HashSet;
    use std::sync::Mutex;

    static VERBOSE_MASK_TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn verbose_categories_have_unique_masks() {
        let mut seen = HashSet::new();
        for category in verbose_category::ALL {
            assert!(
                seen.insert(category.mask),
                "duplicate mask {}",
                category.mask
            );
        }
    }

    #[test]
    fn verbose_categories_have_unique_names() {
        let mut seen = HashSet::new();
        for category in verbose_category::ALL {
            assert!(
                seen.insert(category.name),
                "duplicate name {}",
                category.name
            );
        }
    }

    #[test]
    fn verbose_categories_have_unique_env_names() {
        let mut seen = HashSet::new();
        for category in verbose_category::ALL {
            assert!(
                seen.insert(category.env_name),
                "duplicate env_name {}",
                category.env_name
            );
        }
    }

    #[test]
    fn verbose_categories_round_trip_through_mask_to_env_names() {
        let mask = verbose_category::ALL
            .iter()
            .fold(0u64, |acc, category| acc | category.mask);
        let names = mask_to_env_names(mask);

        for category in verbose_category::ALL {
            assert!(names.contains(&category.env_name));
            assert_eq!(
                verbose_category_by_env_name(category.env_name),
                Some(*category)
            );
        }
    }

    #[test]
    fn parse_verbose_log_mask_accepts_valid_categories_case_insensitively() {
        let mask = parse_verbose_log_mask(" mcp , SCHEDULER , mcp ");
        assert_eq!(
            mask,
            verbose_category::MCP.mask | verbose_category::SCHEDULER.mask
        );
    }

    #[test]
    fn parse_verbose_log_mask_collects_unknown_categories() {
        let (mask, unknowns) =
            parse_verbose_log_mask_with_unknowns("mcp, schedulre, routing, schedulre, nope");
        assert_eq!(
            mask,
            verbose_category::MCP.mask | verbose_category::ROUTING.mask
        );
        assert_eq!(unknowns, vec!["schedulre".to_owned(), "nope".to_owned()]);
    }

    #[test]
    fn verbose_mask_updates_work_as_expected() {
        let _guard = VERBOSE_MASK_TEST_MUTEX.lock().unwrap();
        let original = verbose_log_mask();
        set_verbose_log_mask(0);
        assert_eq!(enable_verbose_category(verbose_category::MCP), 0);
        assert_eq!(verbose_log_mask(), verbose_category::MCP.mask);
        assert_eq!(
            enable_verbose_category(verbose_category::SCHEDULER),
            verbose_category::MCP.mask
        );
        assert_eq!(
            disable_verbose_category(verbose_category::MCP),
            verbose_category::MCP.mask | verbose_category::SCHEDULER.mask
        );
        assert_eq!(
            replace_verbose_log_mask(original),
            verbose_category::SCHEDULER.mask
        );
    }
}
