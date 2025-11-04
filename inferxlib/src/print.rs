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

use chrono::prelude::*;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use lazy_static::lazy_static;
use spin::RwLock;
use std::arch::asm;
use std::fs::OpenOptions;
use std::os::unix::io::IntoRawFd;
use std::string::String;

lazy_static! {
    pub static ref LOG: Log = Log::New();
    pub static ref START_TIME: i64 = Utc::now().timestamp_millis();
    pub static ref LOG_MONITOR: LogMonitor = LogMonitor::New();
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
}

pub fn SetSyncPrint(syncPrint: bool) {
    LOG.SetSyncPrint(syncPrint);
}

pub const LOG_FILE_DEFAULT: &str = "/opt/inferx/log/onenode.log";
pub const RAWLOG_FILE_DEFAULT: &str = "/opt/inferx/log/raw.log";
pub const LOG_FILE_FORMAT: &str = "/opt/inferx/log/{}.log";
pub const TIME_FORMAT: &str = "%H:%M:%S%.3f";

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

        return Self {
            fd: AtomicI32::new(file.into_raw_fd()),
            rawfd: AtomicI32::new(rawfile.into_raw_fd()),
            lineNum: AtomicU64::new(1),
            syncPrint: AtomicBool::new(true),
            processid: AtomicI32::new(std::process::id() as _),
            serviceName: RwLock::new("".to_owned()),
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
