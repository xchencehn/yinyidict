//! 极小的运行期动态库加载器。
//!
//! 为什么不用 `libloading`：它经 `windows-link` 走 `raw-dylib`，在 windows-gnu
//! 工具链上需要 `dlltool.exe`（MinGW binutils），而本机只有 rustup 自带的
//! gnu 工具链。kernel32 有真正的导入库，直接声明这三个函数就绕开了整条链路，
//! 也让整个工程保持零 C 依赖。

use anyhow::{bail, Result};
use std::ffi::{c_void, CString};
use std::path::Path;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

/// 一个已加载的 DLL。析构时释放。
pub struct DynLib {
    handle: *mut c_void,
    name: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl DynLib {
    /// 按名字加载（走系统搜索路径），或按完整路径加载。
    pub fn open(name: &str) -> Result<Self> {
        // SAFETY: 传入以 NUL 结尾的宽字符串，符合 LoadLibraryW 的约定。
        let handle = unsafe { LoadLibraryW(wide(name).as_ptr()) };
        if handle.is_null() {
            // SAFETY: 紧跟失败调用之后读取线程最后错误码。
            let code = unsafe { GetLastError() };
            bail!("加载 {name} 失败，GetLastError={code}");
        }
        Ok(DynLib { handle, name: name.to_string() })
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        let s = path.to_str().unwrap_or_default();
        if s.is_empty() {
            bail!("库路径不是合法 UTF-8: {}", path.display());
        }
        Self::open(s)
    }

    /// 取一个导出符号，转成函数指针类型 `F`。
    ///
    /// # Safety
    /// 调用方必须保证 `F` 与该符号在 DLL 中的真实签名和调用约定一致；
    /// 签名写错会造成未定义行为。返回的指针的有效期不长于 `self`。
    pub unsafe fn sym<F: Copy>(&self, name: &str) -> Result<F> {
        assert_eq!(
            std::mem::size_of::<F>(),
            std::mem::size_of::<*mut c_void>(),
            "F 必须是函数指针大小"
        );
        let c = CString::new(name)?;
        let p = GetProcAddress(self.handle, c.as_ptr() as *const u8);
        if p.is_null() {
            bail!("{} 里没有符号 {name}", self.name);
        }
        Ok(*(&p as *const *mut c_void as *const F))
    }
}

impl Drop for DynLib {
    fn drop(&mut self) {
        // SAFETY: handle 由 LoadLibraryW 得到，且只在这里释放一次。
        unsafe {
            FreeLibrary(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_system_dll_and_finds_a_symbol() {
        let lib = DynLib::open("kernel32.dll").expect("kernel32 应该总能加载");
        // SAFETY: 只检查符号存在，不调用它。
        let f: Result<unsafe extern "system" fn() -> u32> = unsafe { lib.sym("GetLastError") };
        assert!(f.is_ok());
    }

    #[test]
    fn missing_library_and_symbol_report_errors() {
        assert!(DynLib::open("definitely-not-a-real-library.dll").is_err());
        let lib = DynLib::open("kernel32.dll").unwrap();
        let f: Result<unsafe extern "system" fn()> = unsafe { lib.sym("NoSuchExport") };
        assert!(f.is_err());
    }
}
