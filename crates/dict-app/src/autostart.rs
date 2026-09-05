//! 开机自启：往 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 里写一条。
//!
//! **注册表是唯一的事实来源，设置文件里不存这一项。** 用户完全可能绕过我们
//! 去改这个开关 —— 任务管理器的「启动」页、msconfig、别的清理工具都能删掉它。
//! 要是我们自己也记一份，两边迟早对不上，设置页就会显示一个骗人的勾。
//! 所以每次都现读注册表。
//!
//! 用 HKCU 而不是 HKLM：后者要管理员权限，为一个词典要提权是不值当的。

use dict_core::dynlib::DynLib;

/// 注册表里那条值的名字。改名等于旧的那条留在原地永远删不掉，别改。
const VALUE: &str = "yinyidict";
const SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

const HKEY_CURRENT_USER: usize = 0x8000_0001;
const KEY_READ: u32 = 0x2_0019;
const KEY_WRITE: u32 = 0x2_0006;
const REG_SZ: u32 = 1;
const ERROR_SUCCESS: i32 = 0;

type Hkey = usize;

/// advapi32 的五个注册表函数。
///
/// 和工程里其它 Win32 调用一样走 dlopen 而不是链接导入库 —— 这台机器上
/// raw-dylib 要 dlltool，能不添依赖就不添（见 `dict_core::dynlib`）。
struct Advapi {
    _lib: DynLib,
    open: unsafe extern "system" fn(Hkey, *const u16, u32, u32, *mut Hkey) -> i32,
    set: unsafe extern "system" fn(Hkey, *const u16, u32, u32, *const u8, u32) -> i32,
    query:
        unsafe extern "system" fn(Hkey, *const u16, *mut u32, *mut u32, *mut u8, *mut u32) -> i32,
    delete: unsafe extern "system" fn(Hkey, *const u16) -> i32,
    close: unsafe extern "system" fn(Hkey) -> i32,
}

impl Advapi {
    fn open() -> Option<Advapi> {
        let lib = DynLib::open("advapi32.dll").ok()?;
        // SAFETY: 五个符号名和签名都对着 winreg.h 核过。
        unsafe {
            Some(Advapi {
                open: lib.sym("RegOpenKeyExW").ok()?,
                set: lib.sym("RegSetValueExW").ok()?,
                query: lib.sym("RegQueryValueExW").ok()?,
                delete: lib.sym("RegDeleteValueW").ok()?,
                close: lib.sym("RegCloseKey").ok()?,
                _lib: lib,
            })
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前是不是开机自启。读不到就当没开。
pub fn enabled() -> bool {
    let Some(api) = Advapi::open() else { return false };
    let key = wide(SUBKEY);
    let name = wide(VALUE);
    let mut h: Hkey = 0;
    // SAFETY: 打开成功才读，读完必关。
    unsafe {
        if (api.open)(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ, &mut h) != ERROR_SUCCESS {
            return false;
        }
        let mut len: u32 = 0;
        let got = (api.query)(
            h,
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut len,
        );
        (api.close)(h);
        got == ERROR_SUCCESS
    }
}

/// 打开或关掉开机自启。返回是否如愿。
///
/// 写进去的是**当前这个 exe 的绝对路径**，所以把程序挪了地方之后要重新勾一次
/// —— 注册表里那条还指着老位置。这也是为什么值名要固定：换了名字，
/// 老路径那条就成了删不掉的孤儿。
pub fn set(on: bool) -> bool {
    let Some(api) = Advapi::open() else { return false };
    let key = wide(SUBKEY);
    let name = wide(VALUE);
    let mut h: Hkey = 0;
    // SAFETY: 打开成功才写，写完必关。
    unsafe {
        if (api.open)(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_WRITE | KEY_READ, &mut h)
            != ERROR_SUCCESS
        {
            return false;
        }
        let ok = if on {
            match std::env::current_exe() {
                // 路径带空格时不加引号，Windows 会从空格处截断去找 exe
                Ok(exe) => {
                    let v = wide(&format!("\"{}\"", exe.display()));
                    let bytes = v.len() * 2;
                    (api.set)(h, name.as_ptr(), 0, REG_SZ, v.as_ptr() as *const u8, bytes as u32)
                        == ERROR_SUCCESS
                }
                Err(_) => false,
            }
        } else {
            // 本来就没有也算成功 —— 用户要的是「关掉」这个结果
            let r = (api.delete)(h, name.as_ptr());
            r == ERROR_SUCCESS || !enabled_locked(&api, h, &name)
        };
        (api.close)(h);
        ok
    }
}

/// 已经持有句柄时的存在性检查，给 `set` 内部用。
///
/// # Safety
/// `h` 必须是一个还没关闭的、以 `KEY_READ` 打开的句柄。
unsafe fn enabled_locked(api: &Advapi, h: Hkey, name: &[u16]) -> bool {
    let mut len: u32 = 0;
    (api.query)(
        h,
        name.as_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut len,
    ) == ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 开关要能来回切，并且切完读回来是对的。
    ///
    /// 这个测试**会真的动注册表**，所以最后必须还原成跑之前的样子。
    #[test]
    fn toggling_autostart_is_readable_afterwards() {
        let before = enabled();

        assert!(set(true), "打开失败");
        assert!(enabled(), "打开之后读回来应该是开着的");

        assert!(set(false), "关闭失败");
        assert!(!enabled(), "关掉之后读回来应该是关着的");

        // 关掉一个本来就没有的，也该算成功
        assert!(set(false), "重复关闭应该是幂等的");

        set(before);
        assert_eq!(enabled(), before, "测试完要还原成原样");
    }
}
