//! 拿一份微型词库把 ETL 整条跑通，再用检索去验产出。
//!
//! 真实数据有 1.2 GB，CI 上下不动；而 ETL 是这个工程里最容易在重构中悄悄坏掉
//! 的一段（见 CLAUDE.md 里那两次「自我毒化」——都是静默的）。所以 fixture 里
//! 放一份**编出来的**小词库：内容全是假的，不含任何真实词库的片段，
//! 进版本库没有授权问题，却能覆盖真实数据里那几种要命的形态：
//! 多音字（银行）、繁简异体（裡/里）、交叉引用、量词、语域标记、英文屈折。

#![cfg(windows)] // ETL 读 ECDICT 走的是 Windows 自带的 winsqlite3.dll

use dict_core::{Params, Store};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// 跑一次 ETL，把索引产在一个各测试共用的临时目录里。
///
/// **只跑一次**，用 `OnceLock` 卡住。三个测试各建各的索引会撞车：
/// 词库是 mmap 打开的，另一个测试正读着的时候去重写同一份文件，Windows 会给
/// `os error 1224`（「无法在使用用户映射区域打开的文件上执行」）。
/// 而且它们只读，共用一份完全够。
fn index_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
        let out = std::env::temp_dir().join(format!("dict-etl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);

        let st = Command::new(env!("CARGO_BIN_EXE_dict-build"))
            .args(["--data", fixture.to_str().unwrap(), "--out", out.to_str().unwrap()])
            .status()
            .expect("跑不起来 dict-build");
        assert!(st.success(), "ETL 退出码 {st}");
        out
    })
}

/// 打开共用的那份索引。
fn store() -> Store {
    Store::open(index_dir()).expect("索引打不开")
}

#[test]
fn the_whole_pipeline_produces_a_searchable_index() {
    let store = store();
    assert!(store.n > 0, "词库是空的");

    let p = Params::default();

    // 中文词头
    let id = store.lookup_word("测试").expect("查不到「测试」");
    let e = store.entry(id).unwrap();
    assert_eq!(e.word, "测试");
    assert!(!e.senses.is_empty(), "「测试」没有义项");

    // 英文词头
    assert!(store.lookup_word("compiler").is_some(), "查不到 compiler");

    // 拼音通道
    let r = store.search("ceshi", &p);
    assert!(
        r.hits.iter().any(|h| store.entry(h.id).unwrap().word == "测试"),
        "拼音「ceshi」找不到「测试」"
    );

    // 首字母通道
    let r = store.search("byq", &p);
    assert!(
        r.hits.iter().any(|h| store.entry(h.id).unwrap().word == "编译器"),
        "首字母「byq」找不到「编译器」"
    );

    // 英文词头前缀
    let r = store.search("compil", &p);
    assert!(!r.hits.is_empty(), "「compil」什么都没找到");
}

/// 注音必须是词级的。「银行」按字拼会拼出 yin2 xing2，只有词级标注是对的。
#[test]
fn readings_are_disambiguated_per_word_not_per_character() {
    let store = store();
    let id = store.lookup_word("银行").expect("查不到「银行」");
    let e = store.entry(id).unwrap();
    assert!(
        e.reading.contains("háng") || e.reading.contains("hang"),
        "「银行」注音成了 {}，多音字没按词消歧",
        e.reading
    );
}

/// 量词单列一行，不占义项号；语域标记提成字段。
#[test]
fn cedict_markup_is_parsed_out_of_the_gloss_text() {
    let store = store();

    let e = store.entry(store.lookup_word("银行").unwrap()).unwrap();
    assert!(
        !e.senses.iter().any(|s| s.text.starts_with("CL:")),
        "量词跑进义项里了：{:?}",
        e.senses.iter().map(|s| &s.text).collect::<Vec<_>>()
    );

    let e = store.entry(store.lookup_word("将就").unwrap()).unwrap();
    assert!(
        e.senses.iter().any(|s| s.reg.contains("coll")),
        "(coll.) 没被提成语域字段：{:?}",
        e.senses.iter().map(|s| (&s.text, &s.reg)).collect::<Vec<_>>()
    );
}
