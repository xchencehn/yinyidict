//! 只读 SQLite，走 Windows 自带的 `winsqlite3.dll`。
//!
//! 本机没装 MSVC/gcc，任何 `-sys` crate 都编不了。Windows 自己就带了一份
//! SQLite，dlopen 它就能读 ECDICT 的 `stardict.db`，构建期因此保持零 C 依赖。

use anyhow::{anyhow, bail, Result};
use dict_core::dynlib::DynLib;
use std::ffi::{c_char, c_int, c_void, CString};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READONLY: c_int = 1;

type FnOpen = unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type FnPrepare = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    c_int,
    *mut *mut c_void,
    *mut *const c_char,
) -> c_int;
type FnStep = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnColumnText = unsafe extern "C" fn(*mut c_void, c_int) -> *const u8;
type FnColumnCount = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnFinalize = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnClose = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnErrmsg = unsafe extern "C" fn(*mut c_void) -> *const u8;

struct Api {
    prepare: FnPrepare,
    step: FnStep,
    column_text: FnColumnText,
    column_count: FnColumnCount,
    finalize: FnFinalize,
    close: FnClose,
    errmsg: FnErrmsg,
}

pub struct Sqlite {
    // DLL 要活得比这些函数指针久：字段声明顺序决定析构顺序，它必须排在最后销毁，
    // 所以放在最前面的字段会先析构 —— 这里刻意把它放最后。
    api: Api,
    db: *mut c_void,
    _lib: DynLib,
}

/// 从 `*const u8` 读一个以 NUL 结尾的 UTF-8 串；空指针返回空串。
///
/// # Safety
/// `p` 必须为空，或指向一段以 NUL 结尾、在本次调用期间保持有效的内存。
unsafe fn cstr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
}

impl Sqlite {
    pub fn open(path: &str) -> Result<Self> {
        let lib = DynLib::open("winsqlite3.dll")?;
        // SAFETY: 每个签名都照 sqlite3.h 的公开 C ABI 写，
        // winsqlite3.dll 导出的正是这些标准名字（已核对过导出表）。
        let (api, db) = unsafe {
            let open: FnOpen = lib.sym("sqlite3_open_v2")?;
            let api = Api {
                prepare: lib.sym("sqlite3_prepare_v2")?,
                step: lib.sym("sqlite3_step")?,
                column_text: lib.sym("sqlite3_column_text")?,
                column_count: lib.sym("sqlite3_column_count")?,
                finalize: lib.sym("sqlite3_finalize")?,
                close: lib.sym("sqlite3_close")?,
                errmsg: lib.sym("sqlite3_errmsg")?,
            };
            let cpath = CString::new(path)?;
            let mut db: *mut c_void = std::ptr::null_mut();
            let rc = open(cpath.as_ptr(), &mut db, SQLITE_OPEN_READONLY, std::ptr::null());
            if rc != SQLITE_OK {
                bail!("打开 {path} 失败，sqlite rc={rc}");
            }
            (api, db)
        };
        Ok(Sqlite { api, db, _lib: lib })
    }

    fn err(&self) -> String {
        // SAFETY: db 由 open() 建立，在 Drop 之前一直有效。
        unsafe { cstr((self.api.errmsg)(self.db)) }
    }

    /// 逐行执行一条查询，每行以 `&[String]` 交给回调（NULL 变空串）。
    ///
    /// 回调返回 `false` 可提前停止扫描。返回实际读取的行数。
    pub fn each_row(&self, sql: &str, mut f: impl FnMut(&[String]) -> bool) -> Result<usize> {
        let csql = CString::new(sql)?;
        // SAFETY: stmt 在本函数内成对 prepare/finalize；column_text 返回的指针
        // 只在下一次 step 之前有效，这里立刻拷成 String。
        unsafe {
            let mut stmt: *mut c_void = std::ptr::null_mut();
            let rc =
                (self.api.prepare)(self.db, csql.as_ptr(), -1, &mut stmt, std::ptr::null_mut());
            if rc != SQLITE_OK {
                bail!("prepare 失败 rc={rc}: {} —— SQL: {sql}", self.err());
            }

            let ncol = (self.api.column_count)(stmt) as usize;
            let mut row: Vec<String> = vec![String::new(); ncol];
            let mut n = 0usize;
            let mut failure = None;
            loop {
                match (self.api.step)(stmt) {
                    SQLITE_ROW => {
                        for (i, slot) in row.iter_mut().enumerate() {
                            *slot = cstr((self.api.column_text)(stmt, i as c_int));
                        }
                        n += 1;
                        if !f(&row) {
                            break;
                        }
                    }
                    SQLITE_DONE => break,
                    rc => {
                        failure = Some(format!("step 失败 rc={rc}: {}", self.err()));
                        break;
                    }
                }
            }
            // 无论成败都要 finalize，否则 stmt 泄漏
            (self.api.finalize)(stmt);
            match failure {
                Some(msg) => bail!(msg),
                None => Ok(n),
            }
        }
    }

    /// 取单行单列的整数结果，用于 `SELECT COUNT(*)`。
    pub fn scalar_i64(&self, sql: &str) -> Result<i64> {
        let mut out: Option<i64> = None;
        self.each_row(sql, |row| {
            out = row.first().and_then(|s| s.parse().ok());
            false
        })?;
        out.ok_or_else(|| anyhow!("查询没有返回整数结果: {sql}"))
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        // SAFETY: db 仅在此处关闭一次，之后不再被使用。
        unsafe {
            (self.api.close)(self.db);
        }
    }
}
