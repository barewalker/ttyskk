//! 画面の文脈で同音異義語が選び分けられることの見張り。
//!
//! `context-sample.md` は**評価の基準**にする文書。六つの節がそれぞれ違う話題を
//! 持ち、節ごとに違う候補が出るべきところに `▶` の印が置いてある。**答えの漢字は
//! 文書のどこにも書いていない**ので、当たれば注釈の共起語が効いたことになる。
//!
//! ここで固定しておかないと、重みの定数や手掛かりの取り方を触ったときに、**同じ
//! 基準で比べられなくなる**。手で試すときも同じ文書を使う。
//!
//! ```sh
//! env -u TTYSKK_ACTIVE TTYSKK_DEBUG=/tmp/ctx.log ttyskk -- nvim tests/context-sample.md
//! ```
//!
//! **画面いっぱいに文書が見えている前提**で測る。実際の端末では見えている範囲しか
//! 文脈にならないので、手で試すときは巻き上がりに注意する (記録の `画面 …文字` で
//! 何が見えていたか分かる)。

use std::path::PathBuf;

use ttyskk::{context::Context, dict::Dict};

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

/// 節ごとに、`▶` の位置で引いたときに先頭へ来るべき候補。
const EXPECTED: &[(&str, &str, &str)] = &[
    ("一 新聞社の作業", "こうせい", "校正"),
    ("二 English draft", "こうせい", "校正"),
    ("三 劇団の舞台", "こうえん", "公演"),
    ("四 作家をまねく会", "こうえん", "講演"),
    ("五 労働省の統計", "こうせい", "厚生"),
    ("六 計器の調整", "こうせい", "較正"),
];

#[test]
fn each_section_picks_its_own_homophone() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    // **利用者辞書は使わない。** 走らせる人の学習に左右されると基準にならない。
    let dict = Dict::load(&[jisyo], PathBuf::from("/nonexistent/user.dict"), None)
        .expect("辞書を読めない");

    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/context-sample.md"
    ))
    .expect("例文を読めない");

    let mut at = 0usize;
    let mut section = String::new();
    let mut seen = 0usize;
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h.to_string();
        }
        at += line.chars().count();
        if let Some(rest) = line.strip_prefix("▶ ここで ") {
            let reading = match rest.split_whitespace().next().expect("読みが無い") {
                "Kousei" => "こうせい",
                "Kouen" => "こうえん",
                other => panic!("知らない打ち方: {other}"),
            };
            let (want_section, want_reading, want_text) = EXPECTED[seen];
            assert_eq!(section, want_section, "節の並びが変わっている");
            assert_eq!(reading, want_reading, "{section} の読みが変わっている");

            let ctx = Context::new(&text, at);
            let mut cands: Vec<(f64, String)> = dict
                .lookup(reading)
                .into_iter()
                .map(|c| (ctx.score(&c.text, c.annotation.as_deref()), c.text))
                .collect();
            // 点数の降順。同点は辞書の順のまま (安定)。
            cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let (top_score, top_text) = &cands[0];
            assert_eq!(
                top_text, want_text,
                "{section} で {reading} を引いたら {want_text} が先頭に来るはず\n  \
                 上位: {:?}",
                cands.iter().take(3).collect::<Vec<_>>()
            );
            assert!(
                *top_score > 0.0,
                "{section} は手掛かりで選ばれていない (辞書の順で当たっただけ)"
            );
            seen += 1;
        }
        at += 1;
    }
    assert_eq!(seen, EXPECTED.len(), "節の数が変わっている");
}
