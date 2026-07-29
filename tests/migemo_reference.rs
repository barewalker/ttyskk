//! kensaku.vim の実測出力と突き合わせる。
//!
//! **文字列一致は求めない。** kensaku の辞書は migemo-compact-dict、ttyskk の辞書は
//! SKK-JISYO 一式で、候補の集合は必ず違う。ここで測るのは「当たるか」であって
//! 「同じ形か」ではない。
//!
//! - 辞書に依らない分 (打った字面・かな・カタカナ・全角英数・半角カナ) は**必ず当たる**。
//!   ここが落ちるのはローマ字変換か組み立ての退行なので、試験を落とす。
//! - 見出し語は辞書の違いで落ちてよい。**ただし何が落ちたかは出す** — 黙って減ると、
//!   絞り込めているつもりで漏れている形の不具合になる。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

/// kensaku の出力から候補を取り出せなければ、突き合わせる相手がいない。
const REFERENCE: &str = include_str!("kensaku-reference.json");

/// 共有辞書。無ければこの試験は何も測れないので、断って通す。
fn system_jisyo() -> Option<PathBuf> {
    [
        "/usr/share/skk/SKK-JISYO.L",
        "/run/host/usr/share/skk/SKK-JISYO.L",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// `ttyskk migemo` を実際に起こして正規表現を受け取る。
fn migemo(query: &str, flavour: &str, jisyo: &PathBuf) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ttyskk"))
        .args(["migemo", "--flavour", flavour, "--", query])
        .env("TTYSKK_JISYO", jisyo)
        // 利用者辞書と設定は、走らせる人の手元の中身に左右されないよう外す。
        .env("TTYSKK_USER_JISYO", "/nonexistent/user.dict")
        .env("TTYSKK_CONFIG", "/nonexistent/config.toml")
        .output()
        .expect("ttyskk migemo を起こせた");
    assert!(
        out.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("正規表現は UTF-8")
        .trim_end()
        .to_string()
}

/// 正規表現を、当たる文字列の一覧に開き直す。
///
/// kensaku の出力は共通接頭辞がまとめてあるので (`海[技魚]`)、そのままでは
/// 「何に当たるべきか」が読めない。群と文字クラスを開いて平らに戻す。
fn expand(rx: &str) -> Vec<String> {
    let chars: Vec<char> = rx.chars().collect();
    let mut i = 0;
    let out = alternatives(&chars, &mut i);
    assert_eq!(i, chars.len(), "{rx} を最後まで読めた");
    out
}

fn alternatives(s: &[char], i: &mut usize) -> Vec<String> {
    let mut out = sequence(s, i);
    while *i < s.len() && s[*i] == '|' {
        *i += 1;
        out.extend(sequence(s, i));
    }
    out
}

fn sequence(s: &[char], i: &mut usize) -> Vec<String> {
    let mut acc = vec![String::new()];
    while *i < s.len() && s[*i] != '|' && s[*i] != ')' {
        let parts: Vec<String> = match s[*i] {
            // 群 `(?:` … `)`
            '(' => {
                *i += 3;
                let inner = alternatives(s, i);
                *i += 1; // `)`
                inner
            }
            // 文字クラス `[` … `]`
            '[' => {
                *i += 1;
                let mut cs = Vec::new();
                while s[*i] != ']' {
                    if s[*i] == '\\' {
                        *i += 1;
                    }
                    cs.push(s[*i].to_string());
                    *i += 1;
                }
                *i += 1;
                cs
            }
            // 逃がした一字は、その字そのもの
            '\\' => {
                *i += 2;
                vec![s[*i - 1].to_string()]
            }
            c => {
                *i += 1;
                vec![c.to_string()]
            }
        };
        acc = acc
            .iter()
            .flat_map(|head| parts.iter().map(move |p| format!("{head}{p}")))
            .collect();
    }
    acc
}

/// 参照の各件を (入力, kensaku が当てる文字列) にほどく。
fn cases() -> Vec<(String, Vec<String>)> {
    let v: serde_json::Value = serde_json::from_str(REFERENCE).expect("参照を読めた");
    v["cases"]
        .as_array()
        .expect("cases は配列")
        .iter()
        .map(|c| {
            let query = c["query"].as_str().expect("query は文字列").to_string();
            let js = c["js"].as_str().expect("js は文字列");
            (query, expand(js))
        })
        .collect()
}

/// 群と文字クラスを開き直せる (この試験そのものが正しく測れることの確認)。
#[test]
fn the_reference_can_be_unfolded() {
    assert_eq!(expand("(?:海[技魚])"), vec!["海技", "海魚"]);
    assert_eq!(expand("(?:買(?:玉|い玉))"), vec!["買玉", "買い玉"]);
    // 逃がした字はその字に戻る
    assert_eq!(expand(r"(?:[\-÷]| … )"), vec!["-", "÷", " … "]);

    // 参照の 21 件すべてが開ける
    let all = cases();
    assert_eq!(all.len(), 21, "参照は 21 件");
    for (query, wants) in &all {
        assert!(!wants.is_empty(), "{query} の候補が空");
    }
}

/// 辞書に依らない分は必ず当たる。
///
/// 打った字面・ひらがな・カタカナ・全角英数・半角カナは、どの辞書を積んでいても
/// 出る。ここが落ちるならローマ字変換か組み立ての側の退行。
#[test]
fn what_does_not_depend_on_the_dictionary_always_matches() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    // (入力, 必ず当たってほしいもの)
    let must: &[(&str, &[&str])] = &[
        ("kaigi", &["kaigi", "かいぎ", "カイギ", "ｋａｉｇｉ", "ｶｲｷﾞ"]),
        (
            "gijiroku",
            &[
                "gijiroku",
                "ぎじろく",
                "ギジロク",
                "ｇｉｊｉｒｏｋｕ",
                "ｷﾞｼﾞﾛｸ",
            ],
        ),
        ("2026", &["2026", "２０２６"]),
        ("Kaigi", &["Kaigi", "かいぎ", "カイギ", "Ｋａｉｇｉ", "ｶｲｷﾞ"]),
        // nn は ん (ローマ字変換を自前で書き直していないこと)
        ("shinnyuu", &["shinnyuu", "しんゆう", "シンユウ", "ｼﾝﾕｳ"]),
        // 拗音と促音
        ("kya", &["kya", "きゃ", "キャ", "ｷｬ"]),
        ("jyuusho", &["jyuusho", "じゅうしょ", "ジュウショ", "ｼﾞｭｳｼｮ"]),
    ];
    for (query, wants) in must {
        let rx = migemo(query, "rg", &jisyo);
        let re = Regex::new(&rx).unwrap_or_else(|e| panic!("{query}: {rx} を組めない: {e}"));
        for w in *wants {
            assert!(re.is_match(w), "{query}: {w} に当たらない ({rx})");
        }
    }
}

/// 空白で区切ると、両方に当たる語だけが残る。
#[test]
fn words_are_joined_in_order() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    let rx = migemo("kaigi roku", "rg", &jisyo);
    let re = Regex::new(&rx).unwrap();
    assert!(re.is_match("かいぎろく"));
    assert!(re.is_match("会議録"));
    assert!(!re.is_match("かいぎ"), "片方だけでは当たらない");
}

/// PATH から実行ファイルを探す。
fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// nvim の `match()` で当ててみる。当たった位置の並びを返す (-1 は当たらない)。
///
/// 正規表現も対象もファイル越しに渡す。`\` や `|` を引数に混ぜると、どこで
/// 逃がすかの話が二重になって確かめたいものが見えなくなる。
fn vim_matches(nvim: &Path, rx: &str, samples: &[&str]) -> Vec<bool> {
    let dir = std::env::temp_dir().join(format!("ttyskk-vim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rx_path = dir.join("rx.vim");
    let sample_path = dir.join("samples.txt");
    std::fs::write(&rx_path, rx).unwrap();
    std::fs::write(&sample_path, samples.join("\n")).unwrap();

    let script = dir.join("check.lua");
    std::fs::write(
        &script,
        format!(
            r#"local rx = vim.fn.readfile('{}')[1]
for _, s in ipairs(vim.fn.readfile('{}')) do
  io.stdout:write(vim.fn.match(s, rx) .. "\n")
end
"#,
            rx_path.display(),
            sample_path.display()
        ),
    )
    .unwrap();

    let out = Command::new(nvim)
        .args([
            "--headless",
            "--clean",
            "-c",
            &format!("luafile {}", script.display()),
            "-c",
            "q",
        ])
        .output()
        .expect("nvim を起こせた");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<i64>().ok())
        .map(|n| n >= 0)
        .collect()
}

/// 二つの方言が、同じ文字列に当たる。
///
/// **片方だけ試して通したことにしない。** rg 方言は ripgrep と同じ regex crate に、
/// vim 方言は nvim の `match()` に、それぞれ実際に食わせる。逃がす文字の集合が
/// 方言で違うので、`-` のように片方だけで特別な意味を持つ字を必ず混ぜる。
#[test]
fn both_flavours_hit_the_same_strings() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    let Some(nvim) = which("nvim") else {
        eprintln!("nvim が無いので vim 方言は飛ばす");
        return;
    };

    // (入力, 当たってほしいもの, 当たってほしくないもの)
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "kaigi",
            &["会議の記録", "かいぎ室", "カイギ", "ｶｲｷﾞ", "kaigi"],
            &["無関係", "ぎかい"],
        ),
        ("kya", &["きゃ", "キャ", "脚立"], &["きや"]),
        ("2026", &["2026年", "２０２６"], &["2025"]),
        // 逃がす文字が方言で分かれるもの
        ("-", &["-", "ー"], &["あ"]),
    ];

    for (query, hits, misses) in cases {
        let rg_rx = migemo(query, "rg", &jisyo);
        let vim_rx = migemo(query, "vim", &jisyo);
        let re = Regex::new(&rg_rx).unwrap_or_else(|e| panic!("{query}: {rg_rx}: {e}"));

        let samples: Vec<&str> = hits.iter().chain(misses.iter()).copied().collect();
        let vim = vim_matches(&nvim, &vim_rx, &samples);
        assert_eq!(vim.len(), samples.len(), "{query}: nvim の答えが揃わない");

        for (i, s) in samples.iter().enumerate() {
            let want = i < hits.len();
            assert_eq!(re.is_match(s), want, "{query}: rg 方言が {s} で食い違う");
            assert_eq!(vim[i], want, "{query}: vim 方言が {s} で食い違う");
        }
    }
}

/// kensaku が当てるものを、どれだけ当てられるか。
///
/// **落ちてよいが、黙って落ちてはいけない。** 数と中身を出す。
#[test]
fn the_reference_is_covered() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };

    let mut total = 0;
    let mut hit = 0;
    let mut report = String::new();
    for (query, wants) in cases() {
        let rx = migemo(&query, "rg", &jisyo);
        let re = Regex::new(&rx).unwrap_or_else(|e| panic!("{query}: {rx} を組めない: {e}"));

        let missed: BTreeSet<&String> = wants.iter().filter(|w| !re.is_match(w)).collect();
        total += wants.len();
        hit += wants.len() - missed.len();
        if !missed.is_empty() {
            let shown: Vec<&str> = missed.iter().take(12).map(|s| s.as_str()).collect();
            report.push_str(&format!(
                "  {query}: {} / {} 件が落ちた: {}{}\n",
                missed.len(),
                wants.len(),
                shown.join(" "),
                if missed.len() > shown.len() {
                    " …"
                } else {
                    ""
                }
            ));
        }
    }
    eprintln!("kensaku の候補 {hit} / {total} 件に当たった");
    if !report.is_empty() {
        eprintln!("落ちたもの (辞書の違いによる分を含む):\n{report}");
    }

    // 辞書が違う以上すべては当たらないが、半分も当たらないなら組み立てが壊れている。
    assert!(
        hit * 2 > total,
        "当たったのは {hit} / {total} 件しかない。辞書の違いでは説明がつかない"
    );
}
